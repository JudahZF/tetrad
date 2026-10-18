//! Capture bridge. `tetrad_capture::session::Session` runs Neural Capture V1 and
//! `tetrad_capture::v2::Nc2Controller` runs V2. This file moves their requests to `Effect`s and
//! their replies back in. Parts without recovered wiring are notices or `Unsupported`.
//!
//! Files go through [`Platform::capture_storage`], SHARC buffers through
//! [`Platform::sharc_memory`] (limited to [`CAPTURE_WINDOWS`]) and the trainer seed through
//! [`Platform::random_u32`]; device code opens no capture file or memory node itself.

use std::cell::RefCell;
use std::io;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use tetrad_capture::entry::{
    CAB_SITE, CAB_STATE_KEY_OFFSET, CabSource, CapturePlacement, DEFAULT_CAB_MODEL, EntryAction,
    GridEdit, Hardware, NC2_CAB_SITE, NC2_CAB_STATE_KEY_OFFSET, NC2_ENABLE, NC2_PLAYER_MODEL,
    NcPreset,
};
use tetrad_capture::orchestration::{
    BufferId, CallerFact, CaptureOrchestrator, Command, Gate, NcState, Notice, Operation,
    ParamWrite,
};
use tetrad_capture::session::{
    CaptureDsp, CaptureLibrary, CapturePaths, CaptureRecord, CaptureServices, CaptureStorage,
    DspError, Entered, INPUT_LEVEL_1, INPUT_LEVEL_2, Input, METER_POLL_MS, MeterRequest,
    MeterSource, Output, SaveInfo, ServiceError, Session, SessionError, SharcMemory, TrainerSeed,
};
use tetrad_capture::stored::{
    device_type_code, device_type_name, instrument_type_code, instrument_type_name,
};
use tetrad_capture::v2::{
    Capture2File, Nc2Controller, Nc2Effect, Nc2Error, Nc2Input, Nc2Route, Nc2Routing, Nc2State,
    Product,
};
use tetrad_catalog::Specs;
use tetrad_core::performance::{Footswitch, StompIndex};
use tetrad_dsp_host::responses::Event as DspEvent;
use tetrad_dsp_host::setup::drivers::capture::capture2_writes;
use tetrad_dsp_protocol::requests::Request;
use tetrad_dsp_protocol::{CoreId, DeploymentTarget, DspCore, ModuleSlot};
use tetrad_engine::{
    AbsentScalars, Column, CompileOptions, Deployment, Globals, InputPort, OutputPort, Pair, ROWS,
    Row, SCENES, SceneIndex, Site, Slot, Update, import_layout,
};
use tetrad_hal::analog::{GainEndpoint, Verification};
use tetrad_hal::codec::Impedance;
use tetrad_hal::hal::Command as HalCommand;
use tetrad_hal::platform::Platform as HalPlatform;
use tetrad_hal::zenjack::{
    Control as JackControl, GainPlan, GroundLift, HeadphoneVolume, Input as JackInput, InputType,
    NcOutput, Setting, Value as JackValue, XlrOut,
};
use tetrad_library::sidecar::Sidecar;
use tetrad_library::stock::{find_capture, saving_destination, store_media};
use tetrad_media::factory::DeviceCipher;
use tetrad_preset::{Model, ModelCatalog, Param, ParamValue, Preset, PresetDocument, SplitterType};
use tetrad_services::cloud::endpoint::{CloudDomain, CloudEndpoint, CloudStage};
use tetrad_settings::capture::{CaptureInputType, CaptureIo, InputImpedance};
use tetrad_settings::device::DeviceSettings;
use tetrad_settings::io::{ConnectorId, output_gain};
use tetrad_settings::permanent::CloudDomain as SavedDomain;
use tetrad_storage::stock::{
    FolderType, NewFolder, ParamEntry, ProductType, StockDb, StockProduct, USER_ROOTS,
};
use tetrad_ui::{AuditionSource, CaptureRequest, UiCommand as ViewCommand};

use tetrad_dsp_host::session::Clock;

use super::control::{nc1_message, nc2_message};
use super::dsp::{self, Dsp};
use super::jacks::InputChange;
use super::preset::{self, DeviceCatalog, PresetError};
use super::services::{CloudRequest, Nc2Download};
use super::{Device, DeviceError, DspCommand, Effect, Platform, Subsystem, Timer};

const NO_SEED: &str = "trainer seed and capture storage: the platform provides none";
const NO_DOMAIN: &str = "cloud domain for the NC2 controller";

#[derive(Debug, thiserror::Error)]
pub enum CaptureError {
    #[error("not supported yet: {0}")]
    Unsupported(&'static str),
    #[error(transparent)]
    Session(#[from] SessionError),
    #[error(transparent)]
    Dsp(#[from] DspError),
    #[error(transparent)]
    Nc2(#[from] Nc2Error),
}

/// One bridge output, in session order.
#[derive(Debug)]
enum Queued {
    Request(Request),
    Entry(Entry),
}

/// Requests and entry plans the session raises. The bridge drains them into `Effect`s after
/// each call, in order. The session owns its `CaptureDsp` and `CaptureServices` by value, so
/// the queue is shared through `Rc`.
#[derive(Debug, Clone, Default)]
pub struct Outbox(Rc<RefCell<Vec<Queued>>>);

impl Outbox {
    fn push(&self, request: Request) {
        self.0.borrow_mut().push(Queued::Request(request));
    }

    fn take(&self) -> Vec<Queued> {
        std::mem::take(&mut self.0.borrow_mut())
    }
}

impl CaptureDsp for Outbox {
    fn allocate(&mut self, id: BufferId, bytes: u32) -> Result<(), DspError> {
        self.push(Request::BufferAllocate {
            core: DspCore::Core0,
            id: id as u32,
            bytes,
        });
        Ok(())
    }

    fn free(&mut self, id: BufferId) -> Result<(), DspError> {
        self.push(Request::BufferFree {
            core: DspCore::Core0,
            id: id as u32,
        });
        Ok(())
    }

    /// Unplaced writes have no target (stock sends slot 0xff); placed ones go below.
    fn set_parameter(&mut self, _write: ParamWrite) -> Result<(), DspError> {
        Err(DspError::NotDelivered)
    }

    /// `0x02` ParameterChange at the node's deployed target (VA 0x51e4d8 → 0x517010).
    fn set_placed_parameter(
        &mut self,
        target: DeploymentTarget,
        write: ParamWrite,
    ) -> Result<(), DspError> {
        self.push(Request::ParameterChange {
            target,
            index: u32::from(write.parameter),
            value_bits: write.value.to_bits(),
        });
        Ok(())
    }

    /// `0x11` SetHeadphoneMixLevel (VA 0x511ae4).
    fn set_headphone_mix(&mut self, mix: u8, level: f32) -> Result<(), DspError> {
        self.push(Request::SetHeadphoneMixLevel {
            mix,
            level_bits: level.to_bits(),
        });
        Ok(())
    }

    /// Only column sources map to a request. Grid nodes need the unrecovered grid (gap G2).
    fn request_meter(&mut self, request: MeterRequest) -> Result<(), DspError> {
        let MeterSource::Column(slot) = request.source else {
            return Err(DspError::NotDelivered);
        };
        let core = DspCore::try_from(request.core).map_err(|_| DspError::NotDelivered)?;
        self.push(Request::MeterRequest {
            core: CoreId::Dsp(core),
            slot,
            word: u32::from(request.meter),
        });
        Ok(())
    }
}

/// `SharcMemory` for a platform without a SHARC window ([`Platform::sharc_memory`]).
#[derive(Debug, Default)]
pub struct UnmappedSharc;

impl SharcMemory for UnmappedSharc {
    fn write(&mut self, _address: u32, _bytes: &[u8]) -> io::Result<()> {
        Err(io::ErrorKind::Unsupported.into())
    }

    fn read(&mut self, _address: u32, _out: &mut [u8]) -> io::Result<()> {
        Err(io::ErrorKind::Unsupported.into())
    }
}

/// Linux RAM on every QC, as one span over both known layouts. The rootfs `zpu.dtb` and
/// `atma.dtb` have one node, `0x8300_0000` + `0x0f00_0000`. A rev5 QC booted by `ramboot`
/// (2026-10-15, S0 records) runs the SPI-flash tree instead: `memory@83000000` reg
/// `0x8200_0000` + `0x0e00_0000`, `/proc/iomem` System RAM `82000000-8fffffff`, kernel code
/// from `0x8200_8000`. Neither layout has a reserved-memory node, so no DSP buffer lies in it.
pub const LINUX_RAM: (u32, u32) = (0x8200_0000, 0x1000_0000);

/// `(base, len)` of the physical ranges a V1 capture transfer may touch (review-2 R4, review
/// r25 Q4): SHARC L2 and the DDR below [`LINUX_RAM`] (`0x8000_0000`–`0x81ff_ffff`), the only
/// DDR the device tree keeps out of Linux. The DSP buffers are SHARC L2 or DDR addresses
/// (capture-orchestration-contracts rule 6); stock does not show which DDR part (I). DDR above
/// Linux RAM is refused: the populated DDR size is not known, and past its end an address can
/// alias onto Linux RAM. Widen only after S2 logs the real buffer addresses.
pub const CAPTURE_WINDOWS: [(u32, u32); 2] = {
    let (ddr, _) = tetrad_loader::soc2::SOC2_DDR;
    let (linux, _) = LINUX_RAM;
    [tetrad_sys::loader::L2, (ddr, linux - ddr)]
};

/// The platform SHARC memory limited to [`CAPTURE_WINDOWS`], as review B4 limits the SOC2
/// copy: a transfer at a DSP buffer address outside them (a wrong reply, Linux RAM) is refused
/// before the platform maps anything. Reads too: they copy into capture files.
#[derive(Debug)]
pub struct CaptureMemory<M>(pub M);

fn check_capture_range(address: u32, len: usize) -> io::Result<()> {
    let end = u64::from(address) + len as u64;
    let inside = CAPTURE_WINDOWS
        .iter()
        .any(|&(base, size)| base <= address && end <= u64::from(base) + u64::from(size));
    if inside {
        return Ok(());
    }
    Err(io::Error::new(
        io::ErrorKind::PermissionDenied,
        format!("capture transfer {address:#x} + {len:#x} is outside the SHARC capture windows"),
    ))
}

impl<M: SharcMemory> SharcMemory for CaptureMemory<M> {
    fn write(&mut self, address: u32, bytes: &[u8]) -> io::Result<()> {
        check_capture_range(address, bytes.len())?;
        self.0.write(address, bytes)
    }

    fn read(&mut self, address: u32, out: &mut [u8]) -> io::Result<()> {
        check_capture_range(address, out.len())?;
        self.0.read(address, out)
    }
}

/// One DSPController reaction (`_onNcStateChanged`, VA 0x528498) or NC2 preset step for
/// the device to run with [`run_entry`]. Stock runs it inside `setNcState` or the NC2
/// controller call; here it is the next effect in order.
#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    /// The V1 state whose entry gate waits for this plan; `None`: NC2 work, no gate.
    pub state: Option<NcState>,
    pub plan: Vec<EntryAction>,
    pub hardware: Hardware,
}

/// DSPController entry services (l-capture §9): every plan becomes an [`Entry`] in the
/// outbox; the state's `EntryServices` gate waits for [`CaptureBridge::entered`].
#[derive(Debug)]
pub struct EntryServices {
    outbox: Outbox,
    hardware: Hardware,
}

impl EntryServices {
    /// `outbox` must be the bridge's: entries reach the device only through it.
    pub fn new(outbox: Outbox, hardware: Hardware) -> Self {
        Self { outbox, hardware }
    }
}

impl CaptureServices for EntryServices {
    /// - `Quiesce` (I) completes at once: stock runs the revert inside `setNcState`
    ///   (`0x528498` case 0); here its [`Entry`] is already in the outbox ahead of the buffer
    ///   frees, so the device runs it first (review w5 R4).
    /// - `DiscardDecision` is held: stock Back asks "Unsaved capture" (`0x3cea8c`) before it
    ///   sets the state. The view shows the confirm while it is held
    ///   ([`CaptureBridge::view`]); `CaptureRequest::Discard` answers it.
    fn request(&mut self, gate: Gate) -> Result<Option<CallerFact>, ServiceError> {
        match gate {
            Gate::Quiesce => Ok(Some(CallerFact::BackendQuiesced)),
            Gate::DiscardDecision => Ok(None),
            _ => Err(ServiceError::Unsupported),
        }
    }

    fn enter(&mut self, state: NcState, plan: &[EntryAction]) -> Result<Entered, ServiceError> {
        self.outbox.0.borrow_mut().push(Queued::Entry(Entry {
            state: Some(state),
            plan: plan.to_vec(),
            hardware: self.hardware,
        }));
        Ok(Entered::Pending)
    }
}

/// Everything a V1 session takes by value. Built once per process.
pub struct Parts<M, S, C> {
    pub memory: M,
    pub storage: S,
    pub services: C,
    pub seed: TrainerSeed,
}

/// The device bridge: SHARC window and capture files from the platform.
pub type Capture = CaptureBridge<Box<dyn SharcMemory>, Box<dyn CaptureStorage>, EntryServices>;

pub struct CaptureBridge<M, S, C> {
    outbox: Outbox,
    parts: Option<Parts<M, S, C>>,
    session: Option<Session<Outbox, M, S, C>>,
    nc2: Option<Nc2Controller>,
    /// A V1 session is open: the meter poll runs every `METER_POLL_MS`.
    metering: bool,
    /// The NC2 meter timer interval (`Nc2Effect::MeterTimer`).
    nc2_meter: Option<Duration>,
    hardware: Hardware,
    /// DSPController members of the capture swap ([`run_entry`]).
    swap: Swap,
    /// The capture preset on the DSP and its deployment (`+0x758`); `None`: the user preset.
    running: Option<Deployment>,
    /// The last NC_Recorder preset (`+0x2db0`): the refiner copies its cab (state 12).
    recorder: Option<Preset>,
    /// The last MicSetup input levels (UI channels 1, 2) for the wizard view.
    levels: [f32; 2],
    /// The stage progress (0..=1) since the last state change, for the wizard view.
    progress: Option<f32>,
    /// The running capture preset's cab parameters (`CaptureView::cab`).
    cab: Vec<tetrad_ui::ParameterView>,
    /// The V1 page switched the capture output on (`+0x1e8`, set on state 6, `0x3d2890`).
    nc_out: bool,
    /// The downloaded NC2 product between download and save or cleanup.
    nc2_product: Option<Nc2Product>,
    /// NC2 saves for the device, which owns the library ([`Device::nc2_input`]).
    nc2_saves: Vec<Nc2Product>,
    /// `enableNC2` ran with 1 (`+0x75d`): a repeat with the same value does nothing.
    nc2_enabled: bool,
    /// The NC2 route state (controller `+0x2c`).
    nc2_routing: Nc2Routing,
}

/// A downloaded NC2 product (`0x204074`).
pub struct Nc2Product {
    pub product_id: String,
    /// The payload as downloaded, still under the NC2 key. I: the record stores it so, as
    /// [`super::media`] decrypts version-2 rawdata with that key.
    pub payload: Vec<u8>,
    /// `Capture2File::sharc_blob` for the Capture 2 block (`0x515a64`).
    pub blob: Arc<[u8]>,
    pub info: SaveInfo,
}

impl Capture {
    pub fn new() -> Self {
        Self::with_parts(None, None)
    }

    /// Parts from the platform (storage, the SHARC window, one random-source value for the
    /// stock seed), the mini variant of the capture presets, and the NC2 cloud host from the
    /// saved cloud domain (inferred: the production cloud API host).
    pub fn from_platform(
        platform: &mut impl Platform,
        board: HalPlatform,
        settings: &DeviceSettings,
    ) -> Self {
        let hardware = if board.is_atma() {
            Hardware::QuadCortexMini
        } else {
            Hardware::QuadCortex
        };
        let memory: Box<dyn SharcMemory> = match platform.sharc_memory() {
            Some(memory) => Box::new(CaptureMemory(memory)),
            None => Box::new(UnmappedSharc),
        };
        let domain = match settings.settings().cloud_domain {
            SavedDomain::Default => CloudDomain::Default,
            SavedDomain::Alternative => CloudDomain::Alternative,
        };
        let host = CloudEndpoint {
            stage: CloudStage::Production,
            domain,
        }
        .cloud_host();
        let host = Some(host.strip_prefix("https://").unwrap_or(&host).to_owned());
        match platform.random_u32().zip(platform.capture_storage()) {
            Some((random, storage)) => Self::with_entry(
                memory,
                storage,
                TrainerSeed::from_random_device(random),
                hardware,
                host,
            ),
            None => Self {
                hardware,
                ..Self::with_parts(None, host)
            },
        }
    }
}

impl<M: SharcMemory, S: CaptureStorage> CaptureBridge<M, S, EntryServices> {
    /// A bridge whose DSPController entries go out as [`Effect::CaptureEntry`] in session
    /// order.
    pub fn with_entry(
        memory: M,
        storage: S,
        seed: TrainerSeed,
        hardware: Hardware,
        cloud_domain: Option<String>,
    ) -> Self {
        let mut bridge = Self::with_parts(None, cloud_domain);
        bridge.hardware = hardware;
        bridge.parts = Some(Parts {
            memory,
            storage,
            services: EntryServices::new(bridge.outbox.clone(), hardware),
            seed,
        });
        bridge
    }
}

impl Default for Capture {
    fn default() -> Self {
        Self::new()
    }
}

impl<M, S, C> CaptureBridge<M, S, C>
where
    M: SharcMemory,
    S: CaptureStorage,
    C: CaptureServices,
{
    /// The session is created on first use and kept for the process, as the stock controller
    /// singleton is. `parts` are consumed by that first creation.
    pub fn with_parts(parts: Option<Parts<M, S, C>>, cloud_domain: Option<String>) -> Self {
        Self {
            outbox: Outbox::default(),
            parts,
            session: None,
            nc2: cloud_domain.map(Nc2Controller::new),
            metering: false,
            nc2_meter: None,
            hardware: Hardware::default(),
            swap: Swap::default(),
            running: None,
            recorder: None,
            levels: [0.0; 2],
            progress: None,
            cab: Vec::new(),
            nc_out: false,
            nc2_product: None,
            nc2_saves: Vec::new(),
            nc2_enabled: false,
            nc2_routing: Nc2Routing::default(),
        }
    }

    /// A capture preset runs on the DSP (`+0x88d0`): preset loads skip the DSP push.
    pub fn capture_running(&self) -> bool {
        self.running.is_some()
    }

    /// The running capture preset (`+0x758` when it is not the user preset).
    pub fn running(&self) -> Option<&Deployment> {
        self.running.as_ref()
    }

    /// The NC2 state while the NC2 dialog shows: from the open (`showDialog` `0x3d791c`,
    /// `enableNC2(1)`) to the close (`0x203ca4`, `enableNC2(0)`). I: the dialog shows exactly
    /// while `+0x75d` is set.
    pub fn nc2_dialog(&self) -> Option<Nc2State> {
        self.nc2
            .as_ref()
            .filter(|_| self.nc2_enabled)
            .map(Nc2Controller::state)
    }

    /// The NC2 dialog shows "Neural Capture Saved." (mode 3, `0x3d7dc0`): an NC2 save
    /// (`0x1fef08`) emitted `newCaptureSaved()` and no state change followed.
    pub fn nc2_saved(&self) -> bool {
        self.nc2_dialog().is_some() && self.nc2.as_ref().is_some_and(Nc2Controller::saved)
    }

    /// How often the device should run [`Self::on_meter_timer`]; `None` stops the timer.
    pub fn meter_interval(&self) -> Option<Duration> {
        if self.metering {
            Some(Duration::from_millis(u64::from(METER_POLL_MS)))
        } else {
            self.nc2_meter
        }
    }

    /// `Timer::CaptureMeters`: the V1 poll while a session is open and the NC2 meter tick.
    pub fn on_meter_timer(&mut self) -> Result<Vec<Effect>, CaptureError> {
        let mut effects = Vec::new();
        if self.metering {
            effects.extend(self.on_meter_tick()?);
        }
        if self.nc2_meter.is_some() {
            effects.extend(self.nc2(Nc2Input::MeterTick)?);
        }
        Ok(effects)
    }

    /// Capture wizard requests from the UI. Inferred map onto the V1 session commands:
    /// BEGIN opens, CONTINUE/START CAPTURE go on, BACK TO CALIBRATION goes back, REFINE
    /// toggles refinement, ABORT/EXIT close the dialog (`Self::close_v1`). The "Unsaved
    /// capture" answer completes the held Back decision (`0x3cea8c`). Saves go through
    /// `Device::save_capture`, which owns the library.
    ///
    /// Stock buttons (handler `0x3cea8c`): STOP is button `+0x164`, which toggles refine in
    /// states 13 and 14 (`0x338fe8`) and does nothing elsewhere. START FROM SCRATCH is button
    /// `+0x160`: one diagram step back in 2..=5, else `setNcState(1)` when the controller
    /// holds an exported model (`+0x88`, read by `0x3331b8`) or the state is 6, and the
    /// "Unsaved capture" prompt when not. That is `Command::Back` in every state (the session
    /// answers the discard gate itself while a model is held). RESTART CAPTURE is the NC2
    /// dialog's start from scratch (`onStartFromScratchButtonReleased`, `0x3dadc8`); without
    /// an NC2 run it acts as START FROM SCRATCH (I).
    pub fn on_ui(&mut self, command: &ViewCommand) -> Result<Vec<Effect>, CaptureError> {
        let v1 = self.v1_state();
        let command = match command {
            ViewCommand::Capture {
                request: CaptureRequest::Discard(allowed),
            } => {
                let outputs = self
                    .session()?
                    .handle(Input::Fact(CallerFact::DiscardAllowed(*allowed)))?;
                return self.effects_of(outputs);
            }
            ViewCommand::Capture { request } => match request {
                CaptureRequest::Begin => Command::Open,
                CaptureRequest::Continue | CaptureRequest::StartCapture => Command::Next,
                CaptureRequest::Back | CaptureRequest::BackToCalibration => Command::Back,
                CaptureRequest::Audition(source) => {
                    Command::ToggleAb(*source == AuditionSource::Capture)
                }
                CaptureRequest::Refine => Command::RefineToggle,
                CaptureRequest::Stop if v1 == Some(NcState::ManualRefine) => Command::RefineToggle,
                CaptureRequest::Stop => return Ok(Vec::new()),
                CaptureRequest::Restart
                    if self
                        .nc2
                        .as_ref()
                        .is_some_and(|nc2| nc2.state() != Nc2State::Idle) =>
                {
                    return self.nc2(Nc2Input::StartFromScratch);
                }
                CaptureRequest::StartFromScratch | CaptureRequest::Restart => Command::Back,
                CaptureRequest::Abort | CaptureRequest::Exit => return self.close_v1(),
                _ => {
                    return Err(CaptureError::Unsupported(
                        "capture wizard request without a session command",
                    ));
                }
            },
            _ => return Err(CaptureError::Unsupported("not a capture wizard request")),
        };
        self.command(command)
    }

    fn v1_state(&self) -> Option<NcState> {
        self.session
            .as_ref()
            .map(|session| session.machine().state())
    }

    /// The V1 dialog close (`0x3c9a04`, vtable `+0x68`), also run when both trainers
    /// disengage: outputs muted (`0x876470(1, 1)`), the capture output back to normal if the
    /// page set it (`SetNCOutSel(0,0,3,0,25000)`), the meter poll stopped, then state 0, whose
    /// revert unmutes.
    fn close_v1(&mut self) -> Result<Vec<Effect>, CaptureError> {
        let mut effects = vec![Effect::Dsp(DspCommand::MasterMute(1 | 1 << 1))];
        if std::mem::take(&mut self.nc_out) {
            effects.push(Effect::Hal(HalCommand::nc_output(NcOutput::Normal)));
        }
        effects.extend(self.command(Command::Close)?);
        Ok(effects)
    }

    fn session(&mut self) -> Result<&mut Session<Outbox, M, S, C>, CaptureError> {
        if self.session.is_none() {
            let parts = self
                .parts
                .take()
                .ok_or(CaptureError::Unsupported(NO_SEED))?;
            self.session = Some(
                Session::new(
                    CapturePaths::stock(),
                    self.outbox.clone(),
                    parts.memory,
                    parts.storage,
                    parts.services,
                    parts.seed,
                )?
                .with_hardware(self.hardware),
            );
        }
        self.session
            .as_mut()
            .ok_or(CaptureError::Unsupported(NO_SEED))
    }

    pub fn command(&mut self, command: Command) -> Result<Vec<Effect>, CaptureError> {
        let outputs = self.session()?.handle(Input::Command(command))?;
        match command {
            Command::Open => self.metering = true,
            Command::Close => self.metering = false,
            _ => {}
        }
        self.effects_of(outputs)
    }

    /// DSP responses routed here: `BufferAllocated`, `Meter` and model starts
    /// (`dspModelStartResponseSignal`). While the NC2 meter timer runs, its input-level meters
    /// go to the NC2 controller. Other responses, stray replies, unknown buffer ids and
    /// model starts outside a capture placement are not ours and produce no effects.
    pub fn on_reply(&mut self, event: &DspEvent) -> Result<Vec<Effect>, CaptureError> {
        let input = match *event {
            DspEvent::ModelStart {
                core,
                slot,
                started,
            } => match model_target(core, slot) {
                Some(target) => Input::ModelStart {
                    target,
                    ok: started,
                },
                None => return Ok(Vec::new()),
            },
            DspEvent::ModuleStartFailed { core, slot } => match model_target(core, slot) {
                Some(target) => Input::ModelStart { target, ok: false },
                None => return Ok(Vec::new()),
            },
            DspEvent::BufferAllocated(ref response) => {
                let Some(id) = buffer_id(response.words[1]) else {
                    return Ok(Vec::new());
                };
                Input::BufferAllocated {
                    id,
                    bytes: response.byte_count(),
                    address: response.address(),
                }
            }
            DspEvent::Meter(ref record) => {
                let (Ok(core), Ok(meter)) =
                    (u8::try_from(record.core), u16::try_from(record.index))
                else {
                    return Ok(Vec::new());
                };
                // The NC2 setup meters are the core-0 column-0 input levels it requests
                // (`Nc2Effect::RequestInputMeters`, tetrad-capture `v2.rs` `Nc2Input::Meter`).
                if self.nc2_meter.is_some()
                    && core == 0
                    && record.column == 0
                    && (meter == INPUT_LEVEL_1 || meter == INPUT_LEVEL_2)
                {
                    let value = f32::from_bits(record.value_bits);
                    return self.nc2(Nc2Input::Meter { meter, value });
                }
                Input::Meter {
                    core,
                    meter,
                    value: f32::from_bits(record.value_bits),
                }
            }
            _ => return Ok(Vec::new()),
        };
        let Some(session) = self.session.as_mut() else {
            return Ok(Vec::new());
        };
        let outputs = match session.handle(input) {
            Ok(outputs) => outputs,
            Err(
                SessionError::Unmatched | SessionError::NoPlacement | SessionError::Unplaced(_),
            ) => {
                return Ok(Vec::new());
            }
            Err(error) => return Err(error.into()),
        };
        self.effects_of(outputs)
    }

    /// Meter timer tick (`METER_POLL_MS`): expire unanswered requests, then send this tick's.
    pub fn on_meter_tick(&mut self) -> Result<Vec<Effect>, CaptureError> {
        let session = self.session()?;
        let expired = session.expire_meters()?;
        session.poll_meters()?;
        self.effects_of(expired)
    }

    /// The capture wizard as the UI shows it: V1 state, NC2 state, the last input levels,
    /// the stage progress and the held Back decision ("Unsaved capture").
    pub fn view(&self) -> tetrad_ui::CaptureView {
        let machine = self.session.as_ref().map(Session::machine);
        let unsaved_prompt = machine.is_some_and(|machine| {
            machine.pending_requests().any(|(_, operation, _)| {
                matches!(operation, Operation::AwaitCaller(Gate::DiscardDecision))
            })
        });
        tetrad_ui::CaptureView {
            unsaved_prompt,
            cab: self.cab.clone(),
            ..super::ui::views::capture_view(
                machine.map(CaptureOrchestrator::state),
                self.nc2.as_ref().map(Nc2Controller::state),
                self.levels,
                self.progress,
            )
        }
    }

    /// The device ran an [`Entry`] ([`run_entry`]); this answers the state's gate.
    pub fn entered(
        &mut self,
        state: NcState,
        result: Result<Option<CapturePlacement>, ServiceError>,
    ) -> Result<Vec<Effect>, CaptureError> {
        let outputs = self.session()?.handle(Input::Entered { state, result })?;
        self.effects_of(outputs)
    }

    pub fn save(
        &mut self,
        info: SaveInfo,
        library: &mut (impl CaptureLibrary + ?Sized),
    ) -> Result<Vec<Effect>, CaptureError> {
        let outputs = self.session()?.save(info, library)?;
        self.effects_of(outputs)
    }

    pub fn nc2(&mut self, input: Nc2Input) -> Result<Vec<Effect>, CaptureError> {
        let controller = self
            .nc2
            .as_mut()
            .ok_or(CaptureError::Unsupported(NO_DOMAIN))?;
        let effects = controller.apply(input)?;
        let mut out = Vec::new();
        for effect in effects {
            out.extend(self.map_nc2(effect)?);
        }
        Ok(out)
    }

    fn map_nc2(&mut self, effect: Nc2Effect) -> Result<Vec<Effect>, CaptureError> {
        Ok(match effect {
            Nc2Effect::RequestInputMeters => {
                for meter in [INPUT_LEVEL_2, INPUT_LEVEL_1] {
                    self.outbox.request_meter(MeterRequest {
                        core: 0,
                        source: MeterSource::Column(0),
                        meter,
                    })?;
                }
                self.drain()
            }
            Nc2Effect::MeterTimer(interval) => {
                self.nc2_meter = interval.map(|ms| Duration::from_millis(u64::from(ms)));
                Vec::new()
            }
            Nc2Effect::NcOutSel(on) => vec![Effect::Hal(HalCommand::nc_output(if on {
                NcOutput::Capture
            } else {
                NcOutput::Normal
            }))],
            Nc2Effect::GetJob { url, .. } => vec![Effect::Cloud(CloudRequest::Nc2Job(url))],
            Nc2Effect::SchedulePoll { delay_ms } => vec![Effect::StartTimer {
                timer: Timer::Nc2Poll,
                after: Duration::from_millis(u64::from(delay_ms)),
            }],
            Nc2Effect::DownloadProduct { product_id } => {
                vec![Effect::Cloud(CloudRequest::Nc2Download(product_id))]
            }
            Nc2Effect::DeleteProduct { url, .. } => {
                vec![Effect::Cloud(CloudRequest::Nc2Delete(url))]
            }
            Nc2Effect::SaveProduct { product_id } => match self.nc2_product.take() {
                Some(product) if product.product_id == product_id => {
                    self.nc2_saves.push(product);
                    Vec::new()
                }
                _ => vec![Effect::Notice(
                    Subsystem::Capture,
                    format!("NC2 save: product {product_id} was not downloaded"),
                )],
            },
            Nc2Effect::DropAudition => {
                self.nc2_product = None;
                Vec::new()
            }
            Nc2Effect::InputLevel { channel, level } => {
                if let Some(slot) = self.levels.get_mut(usize::from(channel).wrapping_sub(1)) {
                    *slot = level;
                }
                Vec::new()
            }
            Nc2Effect::Remote(remote) => vec![Effect::ControlNotify(Box::new(nc2_message(remote)))],
            // `0x528de4` returns when `+0x75d` already holds the value.
            Nc2Effect::EnableNc2Preset(on) if on == self.nc2_enabled => Vec::new(),
            Nc2Effect::EnableNc2Preset(on) => {
                self.nc2_enabled = on;
                let plan = if on {
                    NC2_ENABLE.to_vec()
                } else {
                    vec![EntryAction::RevertNc2]
                };
                vec![self.nc2_entry(plan)]
            }
            Nc2Effect::Route(route) => vec![self.nc2_entry(vec![EntryAction::Nc2Route(route)])],
            Nc2Effect::LoadAudition => vec![self.nc2_entry(vec![EntryAction::LoadAudition])],
            Nc2Effect::IoParameter { index, value } => {
                vec![self.nc2_entry(vec![EntryAction::Nc2Io { index, value }])]
            }
            Nc2Effect::CabsimBypass(bypassed) => {
                vec![self.nc2_entry(vec![EntryAction::Nc2CabBypass(bypassed)])]
            }
            // `Nc2Effect::IoParameter` stores each value at once ([`EntryDevice::nc2`]); the
            // device's settings flush stands in for the 500 ms single-shot (`0x1fe998`).
            Nc2Effect::SaveSettings { .. } => Vec::new(),
            Nc2Effect::HideAbDialog => vec![Effect::Notice(
                Subsystem::Capture,
                "NC2 A/B dialog hide (0x3b8320): no tetrad-ui NC2 dialog yet".into(),
            )],
        })
    }

    fn nc2_entry(&self, plan: Vec<EntryAction>) -> Effect {
        Effect::CaptureEntry(Box::new(Entry {
            state: None,
            plan,
            hardware: self.hardware,
        }))
    }

    fn effects_of(&mut self, outputs: Vec<Output>) -> Result<Vec<Effect>, CaptureError> {
        let mut effects = Vec::new();
        for output in outputs {
            match output {
                Output::Notice(notice) => {
                    match notice {
                        // Stock progress meters are percent (the remote rounds them, I).
                        Notice::Progress(value) => self.progress = Some(value / 100.0),
                        Notice::State(state) => {
                            self.progress = None;
                            // 0x3d2890: entering 6 switches the capture output on.
                            if state == NcState::MicSetup {
                                self.nc_out = true;
                                effects.push(Effect::Hal(HalCommand::nc_output(NcOutput::Capture)));
                            }
                        }
                        _ => {}
                    }
                    effects.push(Effect::Notice(Subsystem::Capture, format!("{notice:?}")));
                }
                Output::Remote(update) => {
                    effects.push(Effect::ControlNotify(Box::new(nc1_message(update))));
                }
                // The UI level; Cortex Control gets it as `RemoteUpdate::Parameter` (`0x1a13c4`).
                Output::InputLevel { channel, level } => {
                    if let Some(slot) = self.levels.get_mut(usize::from(channel).wrapping_sub(1)) {
                        *slot = level;
                    }
                }
                Output::TrainersDisengaged => effects.extend(self.close_v1()?),
                Output::Saved { hash, new } => effects.push(Effect::Notice(
                    Subsystem::Capture,
                    format!("capture saved as {hash} (new record: {new})"),
                )),
            }
        }
        effects.extend(self.drain());
        Ok(effects)
    }

    fn drain(&self) -> Vec<Effect> {
        self.outbox
            .take()
            .into_iter()
            .map(|queued| match queued {
                Queued::Request(request) => Effect::Dsp(DspCommand::Request(request)),
                Queued::Entry(entry) => Effect::CaptureEntry(Box::new(entry)),
            })
            .collect()
    }
}

/// Capture library collection 2 (`0x76be70`) over `zenui.db`: the user capture products
/// and the "My Captures" root, got or created as `0x835204(db, 2, 1)` does. Metadata and
/// rawdata use the device cipher (`"encryption": "Local"`), as [`super::media`] reads them.
pub struct StockCaptures<'a> {
    pub db: &'a mut StockDb,
    pub cipher: &'a DeviceCipher,
    /// `creator`: [`Hardware::platform_name`] (`0x497ce0`).
    pub platform: &'static str,
    /// `authorName`: the account user name (`0x30c080`), empty when logged out.
    pub author: &'a str,
    pub now: SystemTime,
    /// The stored `downloadDestinationFolderNC` folder key (`0x0064ee28`); `None` or an
    /// unknown key saves to "My Captures".
    pub destination: Option<&'a str>,
}

impl StockCaptures<'_> {
    /// `0x7ee62c` case 2 for a new record of `0x6fa0c4`. Unset record members stay empty
    /// (`corosVersion`, `authorId`, `cloudUUID`, `description`, `tags`); `gainType` is left
    /// out (`+0x2c` = -1); `version` is the constructor's "1" (`0x1ec070`). I: the `date`
    /// format is the preset one and `parametersBlockLength` is the rawdata length.
    fn metadata(&self, record: &CaptureRecord<'_>) -> Sidecar {
        let mut metadata = Sidecar::new_preset(&record.info.name, self.now, "");
        for key in ["author", "cloudId", "instrument_type"] {
            metadata.fields.remove(key);
        }
        for (key, value) in [
            ("authorName", self.author),
            ("cloudUUID", ""),
            ("creator", self.platform),
            ("encryption", "Local"),
            ("hash", &record.hash),
            ("parametersBlockLength", &record.model.len().to_string()),
            ("path", &format!("{}.cns", record.hash)),
            ("version", "1"),
        ] {
            metadata.set_text(key, value);
        }
        set_kind(&mut metadata, record.info);
        metadata
    }

    fn encrypted(&self, mut bytes: Vec<u8>) -> Vec<u8> {
        self.cipher.apply(&mut bytes);
        bytes
    }

    /// NC2 save (`0x1fef08`, collection 2 add vtable `+0x54`). I: the V1 record with
    /// `version` "2", the payload under its NC2 key as rawdata, and the payload's SHA-256 as
    /// the key; stock's product record writer is not recovered.
    pub fn insert_nc2(&mut self, product: &Nc2Product) -> io::Result<()> {
        use sha2::{Digest, Sha256};
        let hash: String = Sha256::digest(&product.payload)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        let record = CaptureRecord {
            info: &product.info,
            hash,
            model: &product.payload,
        };
        let mut metadata = self.metadata(&record);
        metadata.set_text("version", "2");
        self.put(StockProduct {
            id: record.hash,
            product_type: ProductType::Capture,
            read_only: false,
            raw_data: Some(product.payload.clone()),
            metadata: Some(self.encrypted(metadata.encode())),
        })
    }

    /// The record into the saving destination (`saving_destination`: the stored folder, else
    /// "My Captures", made on first use), linked to the root too, under the 2,048-item cap
    /// (`store_media`).
    fn put(&mut self, product: StockProduct) -> io::Result<()> {
        let (product_type, name) = USER_ROOTS[0];
        let root = NewFolder {
            product_type,
            folder_type: FolderType::UserRoot,
            name,
            parent: 0,
            read_only: false,
        };
        self.db
            .transaction(|write| write.ensure_folder(&root))
            .map_err(io::Error::other)?;
        let folder = saving_destination(&self.db.view(), ProductType::Capture, self.destination)
            .map_err(io::Error::other)?;
        store_media(self.db, &product, folder).map_err(io::Error::other)?;
        Ok(())
    }
}

/// `deviceType` and `instrumentType` (`0x7ecc18`, `0x7ec4c4`).
fn set_kind(metadata: &mut Sidecar, info: &SaveInfo) {
    metadata.set_text("deviceType", device_type_name(info.device));
    metadata.set_text("instrumentType", instrument_type_name(info.instrument));
}

/// One NC2 IO parameter into its `[NeuralCapture2ControllerSettings]` key (`0x1ffe80`
/// names). Levels 1 and 6 are not stored. TYPE below 0.5 also clears PHANTOM (`0x1ff730`).
fn store_nc2_io(io: &mut CaptureIo, index: usize, value: f32) {
    let on = value >= 0.5;
    let slot = usize::from(matches!(index, 7..=9 | 11 | 14));
    match index {
        0 => io.return_level = value,
        5 => io.in_level = value,
        12 => io.hp_level = value,
        2 | 7 => {
            io.input_type[slot] = if on {
                CaptureInputType::Mic
            } else {
                io.input_phantom[slot] = false;
                CaptureInputType::Instrument
            };
        }
        3 | 8 => io.input_ground_lift[slot] = on,
        4 | 9 => io.input_phantom[slot] = on,
        10 | 11 => io.output_ground_lift[slot] = on,
        13 | 14 => {
            let step = (value * 8.0).round() as i32;
            if let Ok(step) = InputImpedance::from_stock(step) {
                io.input_impedance[slot] = step;
            }
        }
        _ => {}
    }
}

impl CaptureLibrary for StockCaptures<'_> {
    fn contains(&mut self, hash: &str) -> io::Result<bool> {
        let found = find_capture(&self.db.view(), None, hash).map_err(io::Error::other)?;
        Ok(found.is_some())
    }

    fn insert(&mut self, record: &CaptureRecord<'_>) -> io::Result<()> {
        let product = StockProduct {
            id: record.hash.clone(),
            product_type: ProductType::Capture,
            read_only: false,
            raw_data: Some(self.encrypted(record.model.to_vec())),
            metadata: Some(self.encrypted(self.metadata(record).encode())),
        };
        self.put(product)
    }

    /// `0x6f824c` on a user record: name, device and instrument; other keys stay.
    fn update(&mut self, hash: &str, info: &SaveInfo) -> io::Result<()> {
        let product = self
            .db
            .view()
            .product(hash)
            .map_err(io::Error::other)?
            .ok_or(io::ErrorKind::NotFound)?;
        let mut plain = product.metadata.unwrap_or_default();
        self.cipher.apply(&mut plain);
        let mut metadata = Sidecar::parse(&plain).map_err(io::Error::other)?;
        metadata.set_text("name", &info.name);
        set_kind(&mut metadata, info);
        let bytes = self.encrypted(metadata.encode());
        self.db
            .transaction(|write| write.set_product_metadata(hash, Some(&bytes)))
            .map_err(io::Error::other)
    }
}

fn model_target(core: u32, slot: u32) -> Option<DeploymentTarget> {
    Some(DeploymentTarget {
        core: DspCore::try_from(u8::try_from(core).ok()?).ok()?,
        slot: ModuleSlot::try_from(slot).ok()?,
    })
}

/// Buffer ids of `dspBufferResponseSignal`; the id word is class I (see the doc).
fn buffer_id(word: u32) -> Option<BufferId> {
    Some(match word {
        0 => BufferId::Input,
        1 => BufferId::Target,
        2 => BufferId::TrainerParameters,
        3 => BufferId::RefinerStimulus,
        4 => BufferId::RefinerCapture,
        5 => BufferId::RefinerExport,
        6 => BufferId::TrainerComms,
        _ => return None,
    })
}

/// Lane output control (23000) MUTE and SOLO (VA 0x520498, 0x520540, `0x510e4c`).
pub use tetrad_capture::entry::{LANE_MUTE, LANE_SOLO};
/// `usleep(100000)` after each input override call of the revert (VA 0x528784).
pub const OVERRIDE_SETTLE: Duration = Duration::from_millis(100);
/// Metronome driver slot 4 (Enabled) and the `0x44` destination core 4 (VA 0x557b94).
const METRONOME_ENABLED: u32 = 4;
const METRONOME_DESTINATION: u8 = 4;

/// `usleep(10000)` and `usleep(250000)` around the NC2 input type switch (VA 0x1ff730
/// cases 2 and 7).
const TYPE_MUTE_SETTLE: Duration = Duration::from_millis(10);
const TYPE_SWITCH_SETTLE: Duration = Duration::from_millis(250);

/// A recovered `(row, column)` grid site.
fn grid_site((row, column): (u8, u8)) -> Site {
    Site::Slot(Slot {
        row: Row::new(row).expect("capture site row"),
        column: Column::new(column).expect("capture site column"),
    })
}

/// The V1 capture cab site (1, 4) in a capture deployment.
fn cab_site() -> Site {
    grid_site(CAB_SITE)
}

fn rows() -> [Row; ROWS] {
    std::array::from_fn(|index| Row::new(index as u8).expect("row in range"))
}

/// What runs on the DSP while capture owns it (`+0x758`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CapturePreset {
    /// The stock new preset (`0x51a370`), loaded on 16 → 1.
    Empty,
    Nc(NcPreset),
}

/// One lane's MUTE and SOLO as stock saves them (`+0xb894 + row`, `+0xb898 + row`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct LaneState {
    pub mute: bool,
    pub solo: bool,
}

/// DSPController members the swap keeps between entries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Swap {
    /// `+0x758`; `None`: the user preset (`this+0x20`) is active.
    pub active: Option<CapturePreset>,
    /// The user lane MUTE/SOLO saved on capture entry (`0x50e818`).
    pub lanes: Option<[LaneState; ROWS]>,
    /// Compiler force-disables of Input Gate and Global EQ (`+0x3409`, `+0x3411`).
    pub locked: bool,
}

/// A stomp in DSPController's held map (`+0xf034`: the press `0x5239d0` adds it, the
/// release `0x523f80` takes it out) and whether its switch is down now (the HAL pressed
/// mask `0x509b4c` `+0xc` through the logical-to-physical table at `0xa4e1e8`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HeldStomp {
    pub stomp: StompIndex,
    pub down: bool,
}

/// The stock calls of `_onNcStateChanged` (VA 0x528498) and of the NC2 preset work that
/// need the device. [`run_entry`] orders them; the device runs them over its DSP bridge.
pub trait EntryDevice {
    /// StoreCab: the (1, 4) cab of the running capture preset to `NeuralCaptureDialogSettings`
    /// `ABingHash`/`ABingBypass` (`0x64e880`, `0x64e98c`) and its state to the `default_param`
    /// rows of model + 10^7 (`0x6684dc`).
    fn store_cab(&mut self) -> Result<(), DeviceError>;
    /// `0x20` to cores 0 then 1, `mute | flag << 1` (`0x876470`).
    fn master_mute(&mut self, mute: bool, flag: bool) -> Result<(), DeviceError>;
    /// The update install holds the master mute ([`Services::inputs_masked`]). Default: no
    /// install runs.
    ///
    /// [`Services::inputs_masked`]: super::services::Services::inputs_masked
    fn install_hold(&self) -> bool {
        false
    }
    /// `0x44` metronome Enabled (slot 4) to core 4, sent at once (`0x557b94(drv, 1, !on, 0)`).
    fn metronome(&mut self, on: bool) -> Result<(), DeviceError>;
    /// `0x512e0c`: the stored metronome Enabled again (global or preset tempo block).
    fn restore_metronome(&mut self) -> Result<(), DeviceError>;
    /// Lane MUTE/SOLO of the user preset in its current scene (`0x50e818`).
    fn lane_states(&mut self) -> Result<[LaneState; ROWS], DeviceError>;
    /// One lane output control single edit on the preset that runs (`0x51e4d8`).
    fn lane_edit(&mut self, row: Row, parameter: u32, value: f32) -> Result<(), DeviceError>;
    /// `0x06` SetOutputGain, not stored as the user level (`0x511a38` flag 1).
    fn output_gain(&mut self, output: u8, gain: f32) -> Result<(), DeviceError>;
    /// The stored monitor level again (`0x511a38([+0xb89c], out, 1)`).
    fn restore_monitor_gain(&mut self) -> Result<(), DeviceError>;
    /// Builds and loads `preset`; `locked` disables Input Gate and Global EQ in its compile.
    /// Returns its grid map (`+0xe38`). Stock `0x5122c0(.., 0, 0, ..)`: no capture unmute.
    fn deploy(
        &mut self,
        preset: CapturePreset,
        locked: bool,
    ) -> Result<CapturePlacement, DeviceError>;
    /// IO controller override (`0x7d6440`).
    fn input_override(&mut self, on: bool) -> Result<(), DeviceError>;
    /// The Cortex Control message 42 pair after a lock change (`0x582db8`, `0x582d08`).
    /// Default: no Control link.
    fn lock_changed(&mut self, locked: bool) -> Result<(), DeviceError> {
        let _ = locked;
        Ok(())
    }
    /// `0x527d80(user, 1 or 2, 0)`: the user preset again, then the tempo resend
    /// (`0x512c74`). The load sets the "preset loaded" flag `+0xf030`, which makes the next
    /// momentary release do nothing; `release_armed` (`+0xf031`, a held stomp is down)
    /// clears both flags instead (VA 0x528004).
    fn reload_user(&mut self, release_armed: bool) -> Result<(), DeviceError>;
    /// The held map `+0xf034` in key order, each with its switch state.
    fn held_stomps(&mut self) -> Vec<HeldStomp>;
    /// `0x524358` for one held stomp whose switch is up: `+0xf030 = 0`, then the release
    /// `0x523f80`.
    fn release_stomp(&mut self, stomp: StompIndex) -> Result<(), DeviceError>;
    /// The NC2-only actions: [`EntryAction::Nc2Route`], [`EntryAction::Nc2Io`],
    /// [`EntryAction::Nc2CabBypass`] and [`EntryAction::LoadAudition`].
    fn nc2(&mut self, action: EntryAction) -> Result<(), DeviceError>;
    fn sleep(&mut self, duration: Duration);
    /// A stock step without recovered behavior; reported, never guessed.
    fn gap(&mut self, step: &'static str);
}

/// Runs one DSPController reaction in stock order (VA 0x528498; l-capture §9, w2c "w4
/// capture"). The first failure stops the plan. Returns the grid map of the last deploy.
///
/// `SuspendExpressions` (`expPluggedChanged` disconnect) needs no call: while a capture
/// preset runs the DSP bridge refuses every user-deployment push, expression updates
/// included, and capture pushes carry no expression ([`swap_context`]). The revert
/// (`RevertNormal`) runs all its steps and reports the first error. The `0x6a8244` standby guard of the unmute has no tetrad-app source: it never
/// skips.
pub fn run_entry(
    plan: &[EntryAction],
    hardware: Hardware,
    swap: &mut Swap,
    device: &mut impl EntryDevice,
) -> Result<Option<CapturePlacement>, DeviceError> {
    let mut placement = None;
    for action in plan {
        match *action {
            EntryAction::StoreCab => device.store_cab()?,
            EntryAction::MasterMute { mute, flag } => entry_mute(device, mute, flag)?,
            EntryAction::MetronomeOff => device.metronome(false)?,
            EntryAction::SuspendExpressions => {}
            // `if (+0x758 == 0) 0x50e818`: only the user preset's lanes are kept.
            EntryAction::SaveLaneStates if swap.active.is_some() => {}
            EntryAction::SaveLaneStates => swap.lanes = Some(device.lane_states()?),
            EntryAction::MonitorUnity => device.output_gain(hardware.monitor_output(), 1.0)?,
            EntryAction::LoadEmpty => {
                swap.active = Some(CapturePreset::Empty);
                placement = Some(device.deploy(CapturePreset::Empty, swap.locked)?);
            }
            EntryAction::Activate(preset) => swap.active = Some(CapturePreset::Nc(preset)),
            // The locks act through the next compile; a change also tells Cortex Control.
            EntryAction::GridLock(on) => lock(swap, device, on)?,
            EntryAction::Deploy => {
                let active = swap
                    .active
                    .ok_or(DeviceError::Unavailable("an active capture preset"))?;
                placement = Some(device.deploy(active, swap.locked)?);
            }
            EntryAction::ClearLaneMuteSolo => {
                for row in rows() {
                    device.lane_edit(row, LANE_MUTE, 0.0)?;
                    device.lane_edit(row, LANE_SOLO, 0.0)?;
                }
            }
            EntryAction::InputGainOverride(on) => device.input_override(on)?,
            EntryAction::Unmute { flag } => entry_mute(device, false, flag)?,
            EntryAction::RevertNormal => revert(swap, device, true)?,
            EntryAction::RevertNc2 => revert(swap, device, false)?,
            action @ (EntryAction::Nc2Route(_)
            | EntryAction::Nc2Io { .. }
            | EntryAction::Nc2CabBypass(_)
            | EntryAction::LoadAudition) => device.nc2(action)?,
        }
    }
    Ok(placement)
}

/// A capture plan's `0x876470`. Owned (review r15 R8): Cortex Control stays live during an
/// update install and no Control receiver reads the input mask (`0x1ec83c`; readers
/// `0x544534`, `0x50b548`, `0x1fb980`, `0x6a8718`), so stock runs a Control-started capture or
/// NC2 flow and its unmute. Tetrad runs the flow but keeps the install mute.
fn entry_mute(device: &mut impl EntryDevice, mute: bool, flag: bool) -> Result<(), DeviceError> {
    if device.install_hold() {
        return Ok(());
    }
    device.master_mute(mute, flag)
}

/// The lock setters return when the value is unchanged (`0x582db8`, `0x582d08`).
fn lock(swap: &mut Swap, device: &mut impl EntryDevice, on: bool) -> Result<(), DeviceError> {
    if swap.locked == on {
        return Ok(());
    }
    swap.locked = on;
    device.lock_changed(on)
}

/// Case 0 of `0x528498` after StoreCab (`v1`), or `enableNC2(0)` (`0x528de4`), which has no
/// held-stomp steps and no metronome restore. Every stock call returns nothing, so every
/// step runs (`0x528778`–`0x528c98`); this keeps the first error and goes on.
/// `+0x828 = +0x82c` copies a master volume cache that nothing reads, so it has no step here.
fn revert(swap: &mut Swap, device: &mut impl EntryDevice, v1: bool) -> Result<(), DeviceError> {
    let mut first = Ok(());
    let mut keep = |result: Result<(), DeviceError>| {
        if first.is_ok() {
            first = result;
        }
    };
    keep(device.input_override(true));
    device.sleep(OVERRIDE_SETTLE);
    swap.active = None;
    keep(lock(swap, device, false));
    // `0x5287e4`..`0x528cc4`: the first held stomp whose switch is down sets `+0xf031`.
    // O: the switch state is read once here; `0x524358` reads it again after the reload.
    let held = if v1 { device.held_stomps() } else { Vec::new() };
    keep(device.reload_user(held.iter().any(|held| held.down)));
    keep(device.input_override(false));
    device.sleep(OVERRIDE_SETTLE);
    keep(device.restore_monitor_gain());
    // `0x524588`: MUTE (`0x520498`), then SOLO (`0x51e4d8`), row by row. Taken: a later
    // entry that fails before `SaveLaneStates` must not write this session's lanes.
    if let Some(lanes) = swap.lanes.take() {
        for (row, lane) in rows().into_iter().zip(lanes) {
            keep(device.lane_edit(row, LANE_MUTE, f32::from(u8::from(lane.mute))));
            keep(device.lane_edit(row, LANE_SOLO, f32::from(u8::from(lane.solo))));
        }
    }
    keep(entry_mute(device, false, false));
    if v1 {
        for held in held.iter().filter(|held| !held.down) {
            keep(device.release_stomp(held.stomp));
        }
        keep(device.restore_metronome());
    }
    first
}

/// The cab of a capture preset (V1 (1, 4), NC2 (0, 3)): its stored model and bypass per
/// scene.
#[derive(Debug, Clone, PartialEq)]
pub struct CabBlock {
    pub model: Model,
    pub bypass: [bool; SCENES],
}

/// The stock capture presets (l-capture §9): a new preset (`0x51a370`), then the NC_Recorder
/// (`0x51c780`), NC_Trainer (`0x51b5e8`), NC_Refiner (`0x51b1ac`) or `nc2Preset`
/// (`0x51b7e8`) edits: capture nodes on row 0 with their values in every scene (bypassed in
/// every scene when the node says so), lane ports, the row-0 split (splitter TYPE A/B and
/// STEREO, mixer SPLIT MODE, points {2, 0}) or the bare points (NC2 {-1, 2}), and `cab` at
/// the layout's cab site when it has one.
pub fn capture_preset(
    preset: CapturePreset,
    hardware: Hardware,
    specs: &Specs,
    cab: Option<CabBlock>,
) -> Result<Preset, PresetError> {
    let catalog = DeviceCatalog(specs);
    let mut stored = Preset::new_default(tetrad_firmware::running().version, &catalog)?;
    let CapturePreset::Nc(kind) = preset else {
        return Ok(stored);
    };
    let layout = kind.layout(hardware);
    for node in layout.nodes {
        let mut model = default_block(node.model, &catalog)?;
        for &(index, value) in node.values {
            *model.param_mut(index as usize)? = Param::new(ParamValue::Float(value));
        }
        let at = tetrad_preset::Slot {
            row: 0,
            column: usize::from(node.column),
        };
        stored.insert_block(at, model, &catalog)?;
        if node.bypassed {
            stored.block_bypass_mut(at)?.set_bypassed(0, true)?;
        }
    }
    for ports in &layout.ports {
        let chain = stored
            .chains
            .get_mut(usize::from(ports.row))
            .ok_or(PresetError::Unsupported("a capture lane outside the grid"))?;
        chain.in_portid = Some(ports.input);
        chain.out_portid = Some(ports.output);
    }
    // `Preset::new_default` builds the row-0 junction (splitter, mixer, points) as
    // `0x51a370` does, so the junction errors below are defensive only.
    let missing = |what| PresetError::Unsupported(what);
    if let Some(split) = layout.split {
        let selector = match split.splitter_type_step {
            0 => SplitterType::Type0,
            1 => SplitterType::Type1,
            _ => SplitterType::Type2,
        };
        // `0x51c780`: a catalog splitter without parameters is logged and the pair is not
        // put in (`0x2e4108` skipped); the points are still set.
        if let Some(split_type) = catalog.splitter_type(selector) {
            let chain = (stored.chains.first_mut()).ok_or(missing("a row-0 junction"))?;
            let splitter =
                (chain.combined_splitter.first_mut()).ok_or(missing("a row-0 splitter"))?;
            splitter
                .param_mut(0)?
                .set_value(0, ParamValue::Float(split_type))?;
            splitter
                .param_mut(1)?
                .set_value(0, ParamValue::Float(split.splitter_stereo))?;
            (chain.mixer.first_mut())
                .ok_or(missing("a row-0 mixer"))?
                .param_mut(6)?
                .set_value(0, ParamValue::Float(split.mixer_split_mode))?;
        }
    }
    if let Some(row_points) = layout.split.map(|split| split.points).or(layout.points) {
        let points = (stored.chains.first_mut())
            .and_then(|chain| chain.split_control_points.first_mut())
            .ok_or(missing("row-0 split points"))?;
        (points.split, points.mix) = row_points;
    }
    if let (Some(source), Some(cab)) = (layout.cab, cab) {
        let (row, column) = source.site();
        let at = tetrad_preset::Slot {
            row: usize::from(row),
            column: usize::from(column),
        };
        stored.insert_block(at, cab.model, &catalog)?;
        let bypass = stored.block_bypass_mut(at)?;
        if cab.bypass.iter().all(|&scene| scene == cab.bypass[0]) {
            bypass.set_bypassed(0, cab.bypass[0])?;
        } else {
            bypass.set_scene_mode(true, 0)?;
            for (scene, &bypassed) in cab.bypass.iter().enumerate() {
                bypass.set_bypassed(scene, bypassed)?;
            }
        }
    }
    Ok(stored)
}

/// A grid block with catalog defaults (XML parameters, then MUTE).
fn default_block(model_id: u32, catalog: &impl ModelCatalog) -> Result<Model, PresetError> {
    let model = catalog.model(model_id).ok_or(PresetError::Unsupported(
        "a capture model missing from the catalog",
    ))?;
    Ok(Model::block(
        model_id,
        model.params.into_iter().map(|param| param.default),
    ))
}

/// Compiles a capture preset the way `preset::prepare` does, as a fresh deployment in scene
/// 0. Under the capture lock (`locked`) Input Gate and Global EQ are not placed, so the lane
/// routes do not read a disabled gate (stock reroutes around it, `0x581920`, `0x581c08`: I).
/// The enables themselves follow [`dsp::Context::capture_lock`].
pub fn compile(
    preset: &mut Preset,
    specs: &Specs,
    ddc: bool,
    locked: bool,
) -> Result<(Deployment, Update), PresetError> {
    preset.migrate(&DeviceCatalog(specs))?;
    let document = PresetDocument::parse(&preset.to_bytes(), tetrad_preset::Limits::default())?;
    let imported = import_layout(&document.summary()?, specs, AbsentScalars::Proto3Default)?;
    let options = CompileOptions {
        globals: Globals {
            input_gate: !locked,
            global_eq: !locked,
        },
        ddc,
    };
    let scene = SceneIndex::new(0).expect("scene 0");
    Ok(Deployment::new(imported.layout, specs, options, scene)?)
}

/// The push context of the capture swap from the user context. A capture push
/// (`capture`) reads no user expression (stock disconnects `expPluggedChanged` at entry and
/// the capture presets have no assignments) and runs with the tuner engine off (`+0x88d8`
/// is 0 on the Neural Capture screen, `0x3c9b94`). The tuner mute `+0x88e0` stays, as in
/// stock: it acts only through the engine-on tail, so it never reaches the capture lanes.
pub fn swap_context<'a>(
    user: dsp::Context<'a>,
    capture: bool,
    capture_lock: bool,
    nopoll: bool,
) -> dsp::Context<'a> {
    dsp::Context {
        capture_lock,
        nopoll,
        expression: user.expression.filter(|_| !capture),
        tuner_active: user.tuner_active && !capture,
        ..user
    }
}

/// A Cortex Control NeuralCapture (36) UPDATE for the V1 session (`0x19ee00`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NcRemote {
    /// `state` (`0x3cf6a8`) or `toggle_ab_model` (`0x3cf4dc`).
    Command(Command),
    /// `save_info` (`0x3d405c`): the device dialog's save.
    Save(SaveInfo),
}

impl<P: Platform> Device<P> {
    /// [`Effect::CaptureRemote`]: a Cortex Control request for the V1 session; a save takes
    /// the device library path of the UI save.
    pub(super) fn capture_remote(&mut self, remote: NcRemote) -> Result<(), DeviceError> {
        let effects = match remote {
            NcRemote::Command(command) => self.bridges.capture.command(command)?,
            NcRemote::Save(info) => self.save_capture(info)?,
        };
        self.apply(Ok::<_, DeviceError>(effects));
        self.sync_capture_timer();
        Ok(())
    }

    /// [`Effect::CaptureEntry`]: runs the plan ([`run_entry`]), then answers the session. A
    /// failed step is a notice and fails the state (`NcError::Backend`, state 16). NC2 plans
    /// have no state: a failure is a notice only.
    pub(super) fn capture_entry(&mut self, entry: Entry) -> Result<(), DeviceError> {
        let mut swap = self.bridges.capture.swap;
        let result = run_entry(&entry.plan, entry.hardware, &mut swap, self);
        self.bridges.capture.swap = swap;
        let result = result.map_err(|error| {
            self.notice(
                Subsystem::Capture,
                format!("capture entry {:?} stopped: {error}", entry.state),
            );
            ServiceError::Failed
        });
        if let Some(state) = entry.state {
            let effects = self.bridges.capture.entered(state, result);
            self.apply(effects);
        }
        Ok(())
    }

    /// Capture requests from the UI: the save and the Cab parameter edits need the device;
    /// the rest go to the bridge ([`CaptureBridge::on_ui`]).
    pub(super) fn capture_ui(&mut self, command: &ViewCommand) -> Result<Vec<Effect>, DeviceError> {
        match command {
            ViewCommand::SaveCapture {
                name,
                device,
                instrument,
            } => self.save_capture(SaveInfo {
                name: name.clone(),
                device: *device as i32,
                instrument: *instrument as i32,
            }),
            ViewCommand::Capture {
                request: CaptureRequest::CabNudge { parameter, steps },
            } => {
                self.nudge_cab(*parameter, *steps)?;
                Ok(Vec::new())
            }
            command => Ok(self.bridges.capture.on_ui(command)?),
        }
    }

    /// `Effect::Nc2`: the NC2 entry point for Cortex Control message 63 and the cloud
    /// replies. Saves the controller asks for go to the library here.
    ///
    /// `Open` is followed by the stored IO restore (`0x1ffe80`). A cab bypass request reads
    /// block (0, 3) of the running capture preset when one runs (`0x51334c` reads `+0x758`).
    pub(super) fn nc2_input(&mut self, input: Nc2Input) -> Result<(), DeviceError> {
        // Review 2 R5: the record goes into `zenui.db`. Refused before the controller moves
        // on, as the V1 save (`RunOptions::refusal`), not only by the read-only open.
        if input == Nc2Input::Save && !self.options.persist {
            let refused = "bring-up refused an NC2 save without --persist";
            self.notice(Subsystem::Platform, refused.into());
            return Ok(());
        }
        let input = match input {
            Nc2Input::CabsimBypass { bypass, current } => Nc2Input::CabsimBypass {
                bypass,
                current: (self.bridges.capture.running.as_ref())
                    .and_then(|running| {
                        let block = running.layout().instance(grid_site(NC2_CAB_SITE))?;
                        Some(block.bypass.at(running.scene()))
                    })
                    .unwrap_or(current),
            },
            other => other,
        };
        let open = input == Nc2Input::Open;
        let effects = self.bridges.capture.nc2(input);
        self.apply(effects);
        if open {
            let effects = self
                .bridges
                .capture
                .nc2(Nc2Input::RestoreIo(self.nc2_stored_io()));
            self.apply(effects);
        }
        for product in std::mem::take(&mut self.bridges.capture.nc2_saves) {
            if let Err(error) = self.nc2_save(&product) {
                self.notice(Subsystem::Capture, format!("NC2 save failed: {error}"));
            }
        }
        self.sync_capture_timer();
        Ok(())
    }

    /// The product download (`0x7061b0`): decrypted with the NC2 key (DSP boot vector
    /// `0x27`; I: stock's "ZC decrypt" key), parsed as a Capture 2 file and kept for audition
    /// and save. Any failure answers `Downloaded(None)` ("Downloaded capture is corrupt!").
    pub(super) fn nc2_downloaded(&mut self, download: Nc2Download) -> Result<(), DeviceError> {
        let input = match self.nc2_product(download) {
            Ok(product) => {
                let info = product.info.clone();
                self.bridges.capture.nc2_product = Some(product);
                Nc2Input::Downloaded(Some(Product {
                    name: info.name,
                    device: info.device,
                    instrument: info.instrument,
                }))
            }
            Err(reason) => {
                self.notice(
                    Subsystem::Capture,
                    format!("NC2 product rejected: {reason}"),
                );
                Nc2Input::Downloaded(None)
            }
        };
        self.nc2_input(input)
    }

    /// The product record as the cloud product parser reads it (`0x722464`): `name`, and in
    /// `metadata` the `deviceType` and `instrumentType` names (`0x7ec994`, `0x7ec714`); a
    /// missing or unknown name reads as 0 (I). I: the NC2 download uses this parser (its
    /// callers `0x723188`, `0x72391c` are slots; not traced to `0x7061b0`).
    fn nc2_product(&self, download: Nc2Download) -> Result<Nc2Product, String> {
        let cipher = (self.media.as_ref())
            .and_then(|media| media.nc2_cipher())
            .ok_or("the NC2 key (DSP boot vector 0x27) was not received")?;
        let mut plain = download.payload.clone();
        cipher.apply(&mut plain);
        let blob = Arc::from(
            Capture2File::parse(&plain)
                .map_err(|error| error.to_string())?
                .sharc_blob(),
        );
        let product = &download.product;
        let metadata = |key: &str| {
            (product.get("metadata"))
                .and_then(|metadata| metadata.get(key))
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
        };
        let info = SaveInfo {
            name: (product.get("name").and_then(serde_json::Value::as_str))
                .unwrap_or_default()
                .to_owned(),
            device: device_type_code(metadata("deviceType")).unwrap_or_default(),
            instrument: instrument_type_code(metadata("instrumentType")).unwrap_or_default(),
        };
        Ok(Nc2Product {
            product_id: download.product_id,
            payload: download.payload,
            blob,
            info,
        })
    }

    fn nc2_save(&mut self, product: &Nc2Product) -> Result<(), DeviceError> {
        let cipher = (self.media.as_ref())
            .map(|media| media.cipher().clone())
            .or_else(|| self.platform.media_cipher())
            .ok_or(DeviceError::Unavailable("the device media cipher"))?;
        let now = self.platform.wall_clock().ok_or(DeviceError::Unavailable(
            "a wall clock for the capture date",
        ))?;
        let db = (self.state.library.as_mut())
            .and_then(|library| library.database.as_mut())
            .ok_or(DeviceError::Unavailable("zenui.db"))?;
        StockCaptures {
            db,
            cipher: &cipher,
            platform: self.bridges.capture.hardware.platform_name(),
            author: &self.state.settings.settings().account.username,
            now,
            destination: (self.state.settings.settings().library)
                .download_destination_nc
                .as_deref(),
        }
        .insert_nc2(product)
        .map_err(|error| DeviceError::Platform(Box::new(error)))?;
        self.refresh_directory_media();
        Ok(())
    }

    /// `UiCommand::SaveCapture` (`0x3d3f18`): DeviceType and Instrument go unchanged to
    /// `save_info`, the new record (`0x6fa0c4`) and the update (`0x6f824c`) in collection 2
    /// (`0x76be70`, [`StockCaptures`]).
    fn save_capture(&mut self, info: SaveInfo) -> Result<Vec<Effect>, DeviceError> {
        // The media library's cipher (shared key); the platform only without a library.
        let cipher = (self.media.as_ref())
            .map(|media| media.cipher().clone())
            .or_else(|| self.platform.media_cipher())
            .ok_or(DeviceError::Unavailable("the device media cipher"))?;
        let now = self.platform.wall_clock().ok_or(DeviceError::Unavailable(
            "a wall clock for the capture date",
        ))?;
        let db = self
            .state
            .library
            .as_mut()
            .and_then(|library| library.database.as_mut())
            .ok_or(DeviceError::Unavailable("zenui.db"))?;
        let mut library = StockCaptures {
            db,
            cipher: &cipher,
            platform: self.bridges.capture.hardware.platform_name(),
            author: &self.state.settings.settings().account.username,
            now,
            destination: (self.state.settings.settings().library)
                .download_destination_nc
                .as_deref(),
        };
        let effects = self.bridges.capture.save(info, &mut library)?;
        // The new or updated record shows in the picker and Directory lists at once.
        self.refresh_directory_media();
        Ok(effects)
    }

    /// Encoder steps on a parameter of the running capture preset's (1, 4) cab, as the Grid
    /// editor steps a block (I: stock opens the cab editor from the capture block). The cab
    /// state is stored at the next state change (StoreCab).
    fn nudge_cab(&mut self, parameter: u16, steps: i32) -> Result<(), DeviceError> {
        let specs = self.specs()?;
        let Some(running) = &self.bridges.capture.running else {
            return Ok(());
        };
        let Some(block) = running.layout().instance(cab_site()) else {
            return Ok(());
        };
        let index = u32::from(parameter);
        let (Some(spec), Some(value)) = (
            specs
                .model(block.model.0)
                .and_then(|model| model.parameter(index)),
            block.params.get(usize::from(parameter)),
        ) else {
            return Ok(());
        };
        if spec.scale.kind == tetrad_core::conversion::ParameterType::String {
            return Ok(());
        }
        let normalized = value.at(running.scene());
        let value = match super::ui::project::parameter_kind(&spec.scale, normalized) {
            Some(tetrad_ui::ParameterKind::Toggle { on }) => f32::from(u8::from(!on)),
            _ => spec.scale.encoder(normalized, steps),
        };
        let edited = self
            .edit_running(cab_site(), index, value)
            .unwrap_or(Ok(()));
        self.refresh_cab();
        edited
    }

    /// One single edit on the running capture preset (`0x51e4d8`); a capture preset runs
    /// only under the lock (state 1 to the revert). `None`: no capture preset runs.
    fn edit_running(
        &mut self,
        site: Site,
        parameter: u32,
        value: f32,
    ) -> Option<Result<(), DeviceError>> {
        let mut running = self.bridges.capture.running.take()?;
        let result = self
            .specs()
            .and_then(|specs| {
                running
                    .set_parameter(specs, site, parameter, value)
                    .map_err(|error| DeviceError::from(PresetError::from(error)))
            })
            .and_then(|update| self.swap_push(&running, &update, true, true));
        self.bridges.capture.running = Some(running);
        Some(result)
    }

    /// `CaptureView::cab` from the running capture preset (empty without one); IR values
    /// show their library names.
    fn refresh_cab(&mut self) {
        let cab = match (&self.bridges.capture.running, &self.state.specs) {
            (Some(running), Some(specs)) => {
                let position = tetrad_ui::GridPosition::new(CAB_SITE.0, CAB_SITE.1).ok();
                position.map_or_else(Vec::new, |position| {
                    super::ui::project::parameters(
                        running.layout(),
                        position,
                        running.scene(),
                        specs,
                        self.bridges.ui.media(),
                    )
                })
            }
            _ => Vec::new(),
        };
        self.bridges.capture.cab = cab;
    }

    /// One NC2 route ([`Nc2Routing::edits`]). Every stock setter returns nothing, so every
    /// edit runs; the first error is reported.
    fn nc2_route(&mut self, route: Nc2Route) -> Result<(), DeviceError> {
        let cab_bypass = self.state.settings.settings().capture.nc2.cabsim_bypass;
        let hardware = self.bridges.capture.hardware;
        let edits = self
            .bridges
            .capture
            .nc2_routing
            .edits(route, hardware, cab_bypass);
        let mut first = Ok(());
        for edit in edits {
            let result = self.grid_edit(edit);
            if first.is_ok() {
                first = result;
            }
        }
        first
    }

    /// One edit of the running capture preset. O: without one, stock edits the user preset
    /// (`+0x758 == 0`); here the edit is refused.
    fn grid_edit(&mut self, edit: GridEdit) -> Result<(), DeviceError> {
        let row = |row| Row::new(row).map_err(|_| DeviceError::Unavailable("a grid row"));
        let input_port =
            |port| InputPort::from_raw(port).ok_or(DeviceError::Unavailable("a lane input port"));
        let output_port =
            |port| OutputPort::from_raw(port).ok_or(DeviceError::Unavailable("a lane output port"));
        if let GridEdit::HeadphoneMix { mix, level } = edit {
            return Ok(self.entry_dsp()?.request(Request::SetHeadphoneMixLevel {
                mix,
                level_bits: level.to_bits(),
            })?);
        }
        let mut running = (self.bridges.capture.running.take())
            .ok_or(DeviceError::Unavailable("a running capture preset"))?;
        let mut gate = Vec::new();
        let update = self.specs().and_then(|specs| {
            let update = match edit {
                GridEdit::Lane {
                    row: at,
                    parameter,
                    value,
                } => running.set_parameter(specs, Site::OutputControl(row(at)?), parameter, value),
                GridEdit::Mixer {
                    row: at,
                    parameter,
                    value,
                } => {
                    let pair = if at < 2 { Pair::Upper } else { Pair::Lower };
                    running.set_parameter(specs, Site::Mixer(pair), parameter, value)
                }
                GridEdit::Bypass { site, bypassed } => {
                    running.set_bypass(specs, grid_site(site), bypassed)
                }
                GridEdit::InputPort { row: at, port } => {
                    let (at, port) = (row(at)?, input_port(port)?);
                    running.edit(specs, |layout| {
                        layout.lane_mut(at).input = port;
                        Ok(())
                    })
                }
                GridEdit::OutputPort { row: at, port } => {
                    let (at, port) = (row(at)?, output_port(port)?);
                    running.edit(specs, |layout| {
                        layout.lane_mut(at).output = port;
                        Ok(())
                    })
                }
                GridEdit::InputControl {
                    row: at,
                    parameter,
                    value,
                } => running
                    .set_input_gate(specs, row(at)?, parameter, value)
                    .map(|update| {
                        gate = update.gate;
                        Update::Ops(update.ops)
                    }),
                GridEdit::HeadphoneMix { .. } => unreachable!("sent above"),
            };
            update.map_err(|error| DeviceError::from(PresetError::from(error)))
        });
        let result = update.and_then(|update| self.swap_push(&running, &update, true, true));
        self.bridges.capture.running = Some(running);
        result?;
        // `0x51e4d8` case 0x6d60: the gate messages follow the input router ops.
        if !gate.is_empty() {
            self.entry_dsp()?.send_gate(&gate)?;
        }
        Ok(())
    }

    /// NC2 IO parameter `index` (VA 0x1ff730, 0x1ff190), stored in
    /// `[NeuralCapture2ControllerSettings]`. `0x499a60` is the mini, `0x497fd8` an ESS board.
    /// Every HAL write and scheduler request is unverified (readback and commgap 0), as in
    /// both stock bodies and the V1 page `0x3cc0b0`.
    fn nc2_io(&mut self, index: usize, value: f32) -> Result<(), DeviceError> {
        let platform = self.state.platform;
        let (mini, ess) = (platform.is_atma(), platform.has_ess_codec());
        let input = |second: bool| {
            if second {
                JackInput::Two
            } else {
                JackInput::One
            }
        };
        let lift = if value <= 0.5 {
            GroundLift::Grounded
        } else {
            GroundLift::Lifted
        };
        let jack = |control, value| {
            Setting::new(control, value)
                .map(|setting| Effect::Hal(HalCommand::Set(setting, Verification::Unverified)))
                .map_err(|error| DeviceError::Platform(Box::new(error)))
        };
        // The input type write under the external mute (`0x511904`), 10 ms, 250 ms.
        let switch = |input, kind| {
            Ok::<_, DeviceError>(vec![
                Effect::ScheduleInput(InputChange::ExternalMute(true)),
                Effect::Sleep(TYPE_MUTE_SETTLE),
                jack(JackControl::InputType(input), JackValue::InputType(kind))?,
                Effect::Sleep(TYPE_SWITCH_SETTLE),
                Effect::ScheduleInput(InputChange::ExternalMute(false)),
            ])
        };
        let io = self.state.settings.settings().capture.nc2.io.clone();
        let second = matches!(index, 7..=9 | 11 | 14);
        let slot = usize::from(second);
        let effects = match index {
            0 | 5 => vec![Effect::ScheduleInput(InputChange::Gain {
                endpoint: if index == 0 {
                    GainEndpoint::Input1
                } else {
                    GainEndpoint::Input2
                },
                plan: GainPlan::from_unit(platform, value)
                    .map_err(|error| DeviceError::Platform(Box::new(error)))?,
                verification: Verification::Unverified,
            })],
            2 | 7 if !mini && !ess => {
                let kind = match (value >= 0.5, io.input_phantom[slot]) {
                    (false, _) => InputType::Instrument,
                    (true, false) => InputType::Xlr,
                    (true, true) => InputType::XlrPhantom,
                };
                switch(input(second), kind)?
            }
            3 if !mini => vec![jack(
                JackControl::InputGroundLift(input(false)),
                JackValue::GroundLift(lift),
            )?],
            8 => vec![jack(
                JackControl::InputGroundLift(input(true)),
                JackValue::GroundLift(lift),
            )?],
            // `0x511944(input, v > 0.5)` when ESS or TYPE ≥ 0.5. I: it writes TYPE + PHANTOM
            // as one jack type like the I/O page (`0x3594fc`): ESS boards queue it.
            4 | 9
                if (index == 9 || !mini)
                    && (ess || io.input_type[slot] == CaptureInputType::Mic) =>
            {
                let kind = if value > 0.5 {
                    InputType::XlrPhantom
                } else {
                    InputType::Xlr
                };
                if ess {
                    vec![Effect::ScheduleInput(InputChange::Type {
                        input: input(second),
                        kind,
                        verification: Verification::Unverified,
                    })]
                } else {
                    switch(input(second), kind)?
                }
            }
            10 | 11 => {
                let out = if index == 10 {
                    XlrOut::One
                } else {
                    XlrOut::Two
                };
                vec![jack(
                    JackControl::XlrOutGroundLift(out),
                    JackValue::GroundLift(lift),
                )?]
            }
            // `0x7d6500`: ESS boards take DSP `0x58`, the others the HAL volume (mute at 0).
            12 if ess => vec![Effect::Dsp(DspCommand::Request(
                Request::SetHeadphoneOutputLevel {
                    level_bits: output_gain(value).to_bits(),
                },
            ))],
            12 => {
                let volume = if value <= 0.0 {
                    HeadphoneVolume::Muted
                } else {
                    HeadphoneVolume::from_unit(value)
                        .map_err(|error| DeviceError::Platform(Box::new(error)))?
                };
                vec![jack(
                    JackControl::HeadphoneVolume,
                    JackValue::HeadphoneVolume(volume),
                )?]
            }
            13 | 14 if !mini => vec![Effect::ScheduleInput(InputChange::Impedance {
                input: input(second),
                impedance: Impedance::from_unit(value)
                    .map_err(|error| DeviceError::Platform(Box::new(error)))?,
                verification: Verification::Unverified,
            })],
            // Stock has no hardware call for this index on this board.
            _ => Vec::new(),
        };
        for effect in effects {
            match effect {
                Effect::Dsp(command) => self.dsp_command(command)?,
                other => self.execute(other),
            }
        }
        let events = self.state.settings.update(|settings| {
            store_nc2_io(&mut settings.capture.nc2.io, index, value);
        });
        if !events.is_empty() {
            self.schedule_settings_flush();
        }
        Ok(())
    }

    /// `0x515a64`: only while the NC2 preset is active and (0, 0) holds the Capture 2
    /// player; stock logs a miss and goes on.
    fn load_audition(&mut self) -> Result<(), DeviceError> {
        let nc2 = self.bridges.capture.swap.active == Some(CapturePreset::Nc(NcPreset::Nc2));
        let player = Slot {
            row: Row::new(0).expect("row 0"),
            column: Column::new(0).expect("column 0"),
        };
        let placed = (self.bridges.capture.running.as_ref())
            .filter(|running| {
                nc2 && running
                    .layout()
                    .instance(Site::Slot(player))
                    .is_some_and(|block| block.model.0 == NC2_PLAYER_MODEL)
            })
            .and_then(|running| running.plan().blocks.get(&player).copied());
        let blob =
            (self.bridges.capture.nc2_product.as_ref()).map(|product| Arc::clone(&product.blob));
        let (Some(at), Some(blob)) = (placed, blob) else {
            self.notice(
                Subsystem::Capture,
                "NC2 audition not loaded: no NC2 preset or product (0x515a64)".into(),
            );
            return Ok(());
        };
        let target = dsp::target(at)?;
        Ok(self
            .entry_dsp()?
            .execute_writes(capture2_writes(target, blob).to_vec())?)
    }

    /// The stored IO restored by the NC2 open (`0x1ffe80`), in stock order. `0x49a754` (QC,
    /// not mini) adds input 1; `0x497fd8` (ESS) drops the TYPE pair.
    fn nc2_stored_io(&self) -> Vec<(usize, f32)> {
        let io = &self.state.settings.settings().capture.nc2.io;
        let platform = self.state.platform;
        let flag = |on: bool| f32::from(u8::from(on));
        let kind = |kind: CaptureInputType| flag(kind == CaptureInputType::Mic);
        let impedance = |step: InputImpedance| f32::from(step.to_stock() as u8) / 8.0;
        let mut stored = vec![
            (0, io.return_level),
            (5, io.in_level),
            (12, io.hp_level),
            (8, flag(io.input_ground_lift[1])),
            (9, flag(io.input_phantom[1])),
            (10, flag(io.output_ground_lift[0])),
            (11, flag(io.output_ground_lift[1])),
        ];
        if platform.is_qc() {
            if !platform.has_ess_codec() {
                stored.extend([(2, kind(io.input_type[0])), (7, kind(io.input_type[1]))]);
            }
            stored.extend([
                (3, flag(io.input_ground_lift[0])),
                (4, flag(io.input_phantom[0])),
                (13, impedance(io.input_impedance[0])),
                (14, impedance(io.input_impedance[1])),
            ]);
        }
        stored
    }

    fn entry_dsp(&mut self) -> Result<&mut Dsp<P>, DeviceError> {
        self.dsp.as_mut().ok_or(DeviceError::Unavailable("the DSP"))
    }

    fn specs(&self) -> Result<&Specs, DeviceError> {
        self.state
            .specs
            .as_ref()
            .ok_or(DeviceError::Unavailable("the model catalog"))
    }

    /// The cab a new capture preset gets. Recorder: the stored model (`0x64e868`, 0 →
    /// 12001), its state from the user defaults of model + 10^7 (`0x66833c`) over the
    /// catalog defaults, the stored bypass in every scene (`0x64e974`). NC2: the same from
    /// `capture2CabsimId`, model + 2·10^7 and `capture2CabsimBypass` (`0x51b7e8`, `0x20034c`).
    /// Refiner: the recorder's cab block with its 8 scene bypasses (`0x528498` state 12);
    /// without a recorder, 12001 bypassed (`0x51b1ac`).
    fn capture_cab(&mut self, kind: NcPreset) -> Result<Option<CabBlock>, DeviceError> {
        let source = kind.layout(self.bridges.capture.hardware).cab;
        let catalog = DeviceCatalog(self.specs()?);
        let capture = &self.state.settings.settings().capture;
        let (dialog, key_offset) = match source {
            None => return Ok(None),
            Some(CabSource::Stored) => (&capture.nc1, CAB_STATE_KEY_OFFSET),
            Some(CabSource::Nc2Stored) => (&capture.nc2, NC2_CAB_STATE_KEY_OFFSET),
            Some(CabSource::RecorderCopy) => {
                let fallback = CabBlock {
                    model: default_block(DEFAULT_CAB_MODEL, &catalog)?,
                    bypass: [true; SCENES],
                };
                let at = tetrad_preset::Slot {
                    row: usize::from(CAB_SITE.0),
                    column: usize::from(CAB_SITE.1),
                };
                let Some(mut recorder) = self.bridges.capture.recorder.clone() else {
                    return Ok(Some(fallback));
                };
                // Stock copies five members of the recorder's block into the refiner's
                // (`0x2ea1c4`..`0x2eec74`: model id, parameters and three more); this takes
                // the whole stored block (I: the other members are equal in both builders).
                let Some(model) = recorder.block(at).map_err(PresetError::from)?.cloned() else {
                    return Ok(Some(fallback));
                };
                let bypass = recorder.block_bypass_mut(at).map_err(PresetError::from)?;
                let bypass = std::array::from_fn(|scene| bypass.bypassed(scene).unwrap_or(true));
                return Ok(Some(CabBlock { model, bypass }));
            }
        };
        let model_id = match dialog.cabsim_id {
            0 => DEFAULT_CAB_MODEL,
            id => id,
        };
        let bypass = dialog.cabsim_bypass;
        let mut model = default_block(model_id, &catalog)?;
        let defaults =
            match (self.state.library.as_ref()).and_then(|library| library.database.as_ref()) {
                Some(db) => db
                    .view()
                    .default_params(&(model_id + key_offset).to_string())?,
                None => Vec::new(),
            };
        for entry in defaults {
            let value = entry
                .value
                .parse::<f32>()
                .map_or(ParamValue::String(entry.value), ParamValue::Float);
            if let Ok(index) = usize::try_from(entry.index)
                && let Ok(param) = model.param_mut(index)
            {
                param.set_value(0, value).map_err(PresetError::from)?;
            }
        }
        Ok(Some(CabBlock {
            model,
            bypass: [bypass; SCENES],
        }))
    }

    /// One update of a capture (`capture`) or user deployment through the capture swap, with
    /// the compiler capture lock as given ([`swap_context`]). `nopoll` is the marker test of
    /// every deploy; like `QFile::exists`, a failed test reads as absent.
    fn swap_push(
        &mut self,
        deployment: &Deployment,
        update: &Update,
        capture: bool,
        capture_lock: bool,
    ) -> Result<(), DeviceError> {
        let nopoll = self
            .platform
            .marker(super::platform::Marker::NoPoll)
            .unwrap_or(false);
        let (Some(dsp), Some(specs)) = (self.dsp.as_mut(), &self.state.specs) else {
            return Err(DeviceError::Unavailable("the DSP and the catalog"));
        };
        let (media, failures) = super::resolve_media(
            &mut self.media,
            &mut self.platform,
            deployment.plan(),
            &self.state,
        );
        let context = swap_context(
            super::context(&self.state, specs, deployment, media),
            capture,
            capture_lock,
            nopoll,
        );
        let pending = dsp.apply_capture(&mut self.platform, update, &context, capture)?;
        self.note_failures(failures);
        self.note_pending(Subsystem::Dsp, pending);
        Ok(())
    }
}

impl<P: Platform> EntryDevice for Device<P> {
    fn store_cab(&mut self) -> Result<(), DeviceError> {
        let Some(running) = &self.bridges.capture.running else {
            return Ok(());
        };
        let scene = running.scene();
        let Some(cab) = running.layout().instance(cab_site()) else {
            return Ok(());
        };
        let model_id = cab.model.0;
        let bypass = cab.bypass.at(scene);
        let rows: Vec<ParamEntry> = (0..)
            .zip(&cab.params)
            .map(|(index, value)| ParamEntry {
                index,
                value: value.at(scene).to_string(),
                exp: 0,
                exp_trim_max: 0.0,
                exp_trim_min: 0.0,
            })
            .collect();
        let events = self.state.settings.update(|settings| {
            settings.capture.nc1.cabsim_id = model_id;
            settings.capture.nc1.cabsim_bypass = bypass;
        });
        if !events.is_empty() {
            self.schedule_settings_flush();
        }
        // Review 2 R5: refused here, not only by the read-only `zenui.db` of bring-up.
        if !self.options.persist {
            let refused = "bring-up refused the capture cab state rows without --persist";
            self.notice(Subsystem::Platform, refused.into());
            return Ok(());
        }
        // Stock writes the rows and goes on whatever the database answers.
        let key = (model_id + CAB_STATE_KEY_OFFSET).to_string();
        let written = self
            .state
            .library
            .as_mut()
            .and_then(|library| library.database.as_mut())
            .ok_or(DeviceError::Unavailable("zenui.db"))
            .and_then(|db| Ok(db.transaction(|write| write.set_default_params(&key, &rows))?));
        if let Err(error) = written {
            self.notice(
                Subsystem::Capture,
                format!("capture cab state not stored: {error}"),
            );
        }
        Ok(())
    }

    fn master_mute(&mut self, mute: bool, flag: bool) -> Result<(), DeviceError> {
        let value = u32::from(mute) | u32::from(flag) << 1;
        Ok(self.entry_dsp()?.set_master_mute(value)?)
    }

    fn install_hold(&self) -> bool {
        self.bridges.services.inputs_masked()
    }

    fn metronome(&mut self, on: bool) -> Result<(), DeviceError> {
        self.dsp_command(DspCommand::Control(
            tetrad_dsp_protocol::controls::ControlRequest::MetronomeParameter {
                destination: METRONOME_DESTINATION,
                parameter: METRONOME_ENABLED,
                value_bits: f32::from(u8::from(on)).to_bits(),
            },
        ))
    }

    fn restore_metronome(&mut self) -> Result<(), DeviceError> {
        // `0x512e0c` reads the global or preset tempo block; a fresh controller reads the
        // same stores when none was built yet.
        let fresh;
        let metronome = match self.state.metronome.as_ref() {
            Some(metronome) => metronome,
            None => {
                fresh = super::features::tempo_controller(&self.state)?;
                &fresh
            }
        };
        let state = metronome.state();
        let store = match metronome.mode() {
            Ok(crate::tempo::TempoMode::Global) => Some(&state.global),
            Ok(crate::tempo::TempoMode::Preset) => state.scenes.get(state.current_scene),
            Err(_) => None,
        };
        match store.and_then(|store| store[METRONOME_ENABLED as usize]) {
            Some(value) => self.metronome(value >= 0.5),
            None => Err(DeviceError::Unavailable("a stored metronome Enabled")),
        }
    }

    fn lane_states(&mut self) -> Result<[LaneState; ROWS], DeviceError> {
        let current = self
            .state
            .current
            .as_ref()
            .ok_or(DeviceError::Unavailable("a loaded preset"))?;
        let deployment = &current.deployment;
        let scene = deployment.scene();
        Ok(rows().map(|row| {
            let control = deployment.layout().lane(row).output_control.as_ref();
            let flag = |index: usize| {
                control
                    .and_then(|control| control.params.get(index))
                    // `0x50e85c`: `vcmp #0.0`, `movne`.
                    .is_some_and(|value| value.at(scene) != 0.0)
            };
            LaneState {
                mute: flag(LANE_MUTE as usize),
                solo: flag(LANE_SOLO as usize),
            }
        }))
    }

    fn lane_edit(&mut self, row: Row, parameter: u32, value: f32) -> Result<(), DeviceError> {
        let site = Site::OutputControl(row);
        if let Some(result) = self.edit_running(site, parameter, value) {
            return result;
        }
        let specs = self
            .state
            .specs
            .as_ref()
            .ok_or(DeviceError::Unavailable("the model catalog"))?;
        let current = self
            .state
            .current
            .get_mut()
            .as_mut()
            .ok_or(DeviceError::Unavailable("a loaded preset"))?;
        let update = preset::set_parameter(current, specs, site, parameter, value)?;
        let deployment = current.deployment.clone();
        self.swap_push(&deployment, &update, false, false)?;
        // All four lane setters echo the edit (`0x520374`, `0x520404`, `0x520498`, `0x520540`
        // each call `0x25ea1c`).
        let echo = super::control::lane_echo(row, parameter, value);
        self.execute(Effect::ControlNotify(Box::new(echo)));
        Ok(())
    }

    fn output_gain(&mut self, output: u8, gain: f32) -> Result<(), DeviceError> {
        Ok(self.entry_dsp()?.request(Request::SetOutputGain {
            output,
            gain_bits: gain.to_bits(),
        })?)
    }

    /// `[+0xb89c]` is the last unflagged `0x511a38` gain of the monitor output: SEND 1
    /// LEVEL (300) drives output 4, SEND 2 LEVEL (400) output 5 (`0x35476c`).
    fn restore_monitor_gain(&mut self) -> Result<(), DeviceError> {
        let output = self.bridges.capture.hardware.monitor_output();
        let level = match output {
            4 => ConnectorId::Send1Level,
            _ => ConnectorId::Send2Level,
        };
        self.output_gain(output, output_gain(self.state.io.value(level)))
    }

    fn deploy(
        &mut self,
        preset: CapturePreset,
        locked: bool,
    ) -> Result<CapturePlacement, DeviceError> {
        let cab = match preset {
            CapturePreset::Nc(kind) => self.capture_cab(kind)?,
            CapturePreset::Empty => None,
        };
        let specs = self.specs()?;
        let mut stored = capture_preset(preset, self.bridges.capture.hardware, specs, cab)?;
        let ddc = self
            .state
            .settings
            .settings()
            .general
            .dynamic_delay_compensation;
        let (deployment, update) = compile(&mut stored, specs, ddc, locked)?;
        self.swap_push(&deployment, &update, true, locked)?;
        let tempo = super::actions::tempo_bpm(&self.state);
        self.entry_dsp()?.set_tempo(tempo)?;
        let placement = CapturePlacement::new(
            deployment
                .plan()
                .blocks
                .iter()
                .map(|(slot, &at)| {
                    let site = (slot.row.index() as u8, slot.column.index() as u8);
                    Ok((site, dsp::target(at)?))
                })
                .collect::<Result<Vec<_>, dsp::DspError>>()?,
        );
        if preset == CapturePreset::Nc(NcPreset::Recorder) {
            self.bridges.capture.recorder = Some(stored);
        }
        self.bridges.capture.running = Some(deployment);
        self.refresh_cab();
        Ok(placement)
    }

    /// `0x7d6440` sets the one external mute `io+0xc` of the gain scheduler and sends its
    /// trims, or zeros while `+0xc` or `+0xd` is set (review w5 R15).
    fn input_override(&mut self, on: bool) -> Result<(), DeviceError> {
        let effects = self
            .bridges
            .jacks
            .enqueue(InputChange::ExternalMute(on), self.platform.hal())?;
        for effect in effects {
            match effect {
                Effect::Dsp(command) => self.dsp_command(command)?,
                other => self.execute(other),
            }
        }
        Ok(())
    }

    fn lock_changed(&mut self, locked: bool) -> Result<(), DeviceError> {
        let effects = self
            .bridges
            .control
            .capture_lock_changed(locked, &self.state);
        effects.into_iter().for_each(|effect| self.execute(effect));
        Ok(())
    }

    /// The capture deployment is gone first: after a failed push the lane edits of the
    /// revert go to the user preset, and the DSP guard is already clear
    /// ([`Dsp::apply_capture`]).
    fn reload_user(&mut self, release_armed: bool) -> Result<(), DeviceError> {
        self.state.perform.preset_loaded = !release_armed;
        self.bridges.capture.running = None;
        self.refresh_cab();
        let current = self
            .state
            .current
            .as_ref()
            .ok_or(DeviceError::Unavailable("a loaded preset"))?;
        let deployment = current.deployment.clone();
        let Update::Ops(ops) = deployment.clone().select_scene(deployment.scene()) else {
            unreachable!("a scene selection is values only");
        };
        let update = Update::Redeploy {
            clear: Vec::new(),
            plan: Box::new(deployment.plan().clone()),
            ops,
        };
        self.swap_push(&deployment, &update, false, false)?;
        let tempo = super::actions::tempo_bpm(&self.state);
        self.dsp_command(DspCommand::Tempo(tempo))?;
        let metronome = super::actions::reduce(super::Action::MetronomeResend, &mut self.state);
        self.apply(metronome);
        Ok(())
    }

    /// The held-stomp map `+0xf034` with each switch state read now (HAL mask through
    /// `0xa4e1e8`, the logical-to-physical table).
    fn held_stomps(&mut self) -> Vec<HeldStomp> {
        let pressed = self.state.performance.held;
        (self.state.perform.held.iter())
            .map(|&stomp| HeldStomp {
                stomp,
                down: pressed & Footswitch::of_stomp(stomp).physical().mask() != 0,
            })
            .collect()
    }

    /// `+0xf030 = 0`, then the stomp release `0x523f80`: its blocks and its driver hook.
    fn release_stomp(&mut self, stomp: StompIndex) -> Result<(), DeviceError> {
        self.state.perform.preset_loaded = false;
        let effects = super::actions::reduce(super::Action::ReleaseStomp(stomp), &mut self.state);
        self.apply(effects);
        self.dsp_command(DspCommand::Footswitch {
            stomp,
            pressed: false,
        })
    }

    fn nc2(&mut self, action: EntryAction) -> Result<(), DeviceError> {
        match action {
            EntryAction::Nc2Route(route) => self.nc2_route(route),
            EntryAction::Nc2Io { index, value } => self.nc2_io(index, value),
            EntryAction::Nc2CabBypass(bypassed) => {
                let events = self.state.settings.update(|settings| {
                    settings.capture.nc2.cabsim_bypass = bypassed;
                });
                if !events.is_empty() {
                    self.schedule_settings_flush();
                }
                self.grid_edit(GridEdit::Bypass {
                    site: NC2_CAB_SITE,
                    bypassed,
                })
            }
            EntryAction::LoadAudition => self.load_audition(),
            _ => Err(DeviceError::Unavailable("an NC2 entry action")),
        }
    }

    fn sleep(&mut self, duration: Duration) {
        self.clock.sleep(duration);
    }

    fn gap(&mut self, step: &'static str) {
        self.notice(Subsystem::Capture, format!("not done: {step}"));
    }
}

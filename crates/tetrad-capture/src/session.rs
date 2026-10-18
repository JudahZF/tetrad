//! V1 capture session: runs `CaptureOrchestrator` requests through injected backends.
//!
//! Stock sources (ZenUI VAs): stimulus paths 0x6da14, size stat 0x338a1c, file→SHARC 0x333490,
//! SHARC→ARM 0x337184/0x337758, dialog meter poll 0x3cbedc and meter routing 0x3cbbd8.
//! DSPController entry plans come from `crate::entry` (VA 0x528498).

use crate::entry::{CapturePlacement, DspController, EntryAction, Hardware};
use crate::orchestration::{
    BufferId, BufferLease, CallerFact, CaptureOrchestrator, Command, Effect, Epoch, Event,
    FileRole, Gate, NcError, NcState, Notice, Operation, ParamWrite, Refusal, Reply, RequestId,
    Sizes, StockError,
};
use sha2::{Digest, Sha256};
use std::collections::VecDeque;
use std::io;
use std::path::{Path, PathBuf};
use tetrad_dsp_protocol::DeploymentTarget;

/// Directory of the stock stimulus files (VA 0x6da14).
pub const STIMULUS_DIR: &str = "/opt/neuraldsp/sharc_binaries";
/// Capture data directory, global `DAT_015607d0` (`_INIT_596`, VA 0xafd8c).
pub const DATA_DIR: &str = "/media/p4/Captures";

/// Input-level meters polled in MicSetup (VA 0x3cbedc).
pub const INPUT_LEVEL_1: u16 = 0x4000;
pub const INPUT_LEVEL_2: u16 = 0x4001;
/// Dialog meter timer `+0x1cc` (`mov r1,#100` at VA 0x3d3344 before `QTimer::start(int)`).
pub const METER_POLL_MS: u32 = 100;

/// Capture file locations. Stimuli are stock firmware files; outputs live in the data directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapturePaths {
    pub input_stimulus: PathBuf,
    pub refine_stimulus: PathBuf,
    pub refiner_capture: PathBuf,
    pub input_capture: PathBuf,
}

impl CapturePaths {
    /// Stock stimulus paths plus `refiner_capture.bin` and `input_capture.bin` in `DATA_DIR`.
    pub fn stock() -> Self {
        let stimuli = Path::new(STIMULUS_DIR);
        let data_dir = Path::new(DATA_DIR);
        Self {
            input_stimulus: stimuli.join("input.bin"),
            refine_stimulus: stimuli.join("refine.bin"),
            refiner_capture: data_dir.join("refiner_capture.bin"),
            input_capture: data_dir.join("input_capture.bin"),
        }
    }

    pub fn get(&self, role: FileRole) -> &Path {
        match role {
            FileRole::InputStimulus => &self.input_stimulus,
            FileRole::RefineStimulus => &self.refine_stimulus,
            FileRole::RefinerCapture => &self.refiner_capture,
            FileRole::InputCapture => &self.input_capture,
        }
    }
}

/// Capture files. The device backend uses `std::fs`; tests use memory.
pub trait CaptureStorage {
    fn file_len(&mut self, path: &Path) -> io::Result<u64>;
    /// Fills `out` from the start of the file.
    fn read(&mut self, path: &Path, out: &mut [u8]) -> io::Result<()>;
    /// Creates or truncates the file, writes every byte and syncs.
    fn write(&mut self, path: &Path, bytes: &[u8]) -> io::Result<()>;
}

/// Platform backends arrive boxed (`tetrad-app` `Platform::capture_storage`).
impl<T: CaptureStorage + ?Sized> CaptureStorage for Box<T> {
    fn file_len(&mut self, path: &Path) -> io::Result<u64> {
        (**self).file_len(path)
    }
    fn read(&mut self, path: &Path, out: &mut [u8]) -> io::Result<()> {
        (**self).read(path, out)
    }
    fn write(&mut self, path: &Path, bytes: &[u8]) -> io::Result<()> {
        (**self).write(path, bytes)
    }
}

/// `std::fs` capture storage for the device.
#[derive(Debug, Default, Clone, Copy)]
pub struct FsStorage;

impl CaptureStorage for FsStorage {
    fn file_len(&mut self, path: &Path) -> io::Result<u64> {
        Ok(std::fs::metadata(path)?.len())
    }
    fn read(&mut self, path: &Path, out: &mut [u8]) -> io::Result<()> {
        io::Read::read_exact(&mut std::fs::File::open(path)?, out)
    }
    /// Stock opens without truncation and calls `sync()`; truncation here is owned policy.
    fn write(&mut self, path: &Path, bytes: &[u8]) -> io::Result<()> {
        let mut file = std::fs::File::create(path)?;
        io::Write::write_all(&mut file, bytes)?;
        file.sync_all()
    }
}

/// SHARC memory at allocation-reply addresses (`/dev/mem` on the device).
pub trait SharcMemory {
    fn write(&mut self, address: u32, bytes: &[u8]) -> io::Result<()>;
    fn read(&mut self, address: u32, out: &mut [u8]) -> io::Result<()>;
}

/// Platform backends arrive boxed (`tetrad-app` `Platform::sharc_memory`).
impl<T: SharcMemory + ?Sized> SharcMemory for Box<T> {
    fn write(&mut self, address: u32, bytes: &[u8]) -> io::Result<()> {
        (**self).write(address, bytes)
    }
    fn read(&mut self, address: u32, out: &mut [u8]) -> io::Result<()> {
        (**self).read(address, out)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum DspError {
    /// Nothing reached the DSP. Only valid when no part of the request executed.
    #[error("DSP request was not delivered")]
    NotDelivered,
    #[error("DSP request failed")]
    Failed,
}

/// Where a meter is read: a DSP slot, or capture node `n` (grid row 0, column `n`).
/// With a [`CapturePlacement`] the session sends nodes as `Column(slot)` (VA 0x510ba0);
/// without one it passes `Node` through.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MeterSource {
    Column(u8),
    Node(u8),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MeterRequest {
    pub core: u8,
    pub source: MeterSource,
    pub meter: u16,
}

/// Capture requests on the stock ARM↔SHARC protocol.
pub trait CaptureDsp {
    /// `BufferAllocate` 0x0f on core 0, kind 2. The reply is `Input::BufferAllocated`.
    fn allocate(&mut self, id: BufferId, bytes: u32) -> Result<(), DspError>;
    /// `BufferFree` 0x10. `Ok` means the backend executed the free.
    fn free(&mut self, id: BufferId) -> Result<(), DspError>;
    /// `set_param(row 0, node, parameter, f32)` (VA 0x523644) without a placement.
    fn set_parameter(&mut self, write: ParamWrite) -> Result<(), DspError>;
    /// The same write at the node's deployed target (VA 0x510ba0). Stock sends it as a
    /// single edit, which ends in `0x02` ParameterChange (VA 0x51e4d8 → 0x517010).
    /// The default forwards to `set_parameter`.
    fn set_placed_parameter(
        &mut self,
        _target: DeploymentTarget,
        write: ParamWrite,
    ) -> Result<(), DspError> {
        self.set_parameter(write)
    }
    /// `MeterRequest` (DSP vtable +0x3c). The reply is `Input::Meter`.
    fn request_meter(&mut self, request: MeterRequest) -> Result<(), DspError>;
    /// `0x11` SetHeadphoneMixLevel (VA 0x511ae4), raw f32 body. The state-1 tick sends
    /// [`Hardware::start_mixes`] before node1 p2 (VA 0x339320).
    fn set_headphone_mix(&mut self, _mix: u8, _level: f32) -> Result<(), DspError> {
        Err(DspError::NotDelivered)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ServiceError {
    #[error("capture service is not supported by this backend")]
    Unsupported,
    #[error("capture service failed")]
    Failed,
}

/// DSPController and dialog services. The session completes most gates itself: model
/// starts, unmute, displayed progress and the injected `TrainerSeed`. Stock ARM writes no
/// trainer parameters or comms (the `/params.bin` writer VA 0x33ac0c is dead).
pub trait CaptureServices {
    /// Starts the work for `DiscardDecision` or `Quiesce`. `Ok(Some(fact))` completes
    /// now; `Ok(None)` means the fact arrives later through `Input::Fact`.
    fn request(&mut self, gate: Gate) -> Result<Option<CallerFact>, ServiceError>;

    /// Runs one DSPController reaction ([`crate::entry::DspController::enter`]) for `state`,
    /// in order. The session calls it for every state change with a non-empty plan; the
    /// result answers that state's `EntryServices` gate. `Done(Some(placement))` replaces
    /// the deployment map after a `Deploy`. [`Entered::Pending`] means the caller runs the
    /// plan later and reports it with [`Input::Entered`]. Success after an
    /// [`EntryAction::Unmute`] means `modelUmuteSignal` was emitted; report a guarded
    /// (skipped) unmute as `Failed`.
    fn enter(&mut self, _state: NcState, _plan: &[EntryAction]) -> Result<Entered, ServiceError> {
        Err(ServiceError::Unsupported)
    }
}

/// How [`CaptureServices::enter`] handled a plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Entered {
    /// Ran now; `Some` replaces the deployment map.
    Done(Option<CapturePlacement>),
    /// Runs later; the result arrives as [`Input::Entered`] and the state's `EntryServices`
    /// gate waits for it. Stock runs it inside `setNcState` (VA 0x338118 → 0x528498).
    Pending,
}

/// Training seed for nodes 0 and 4 parameter 4 (VA 0x33b02c).
///
/// Stock makes it once per process: the singleton controller (`DAT_015480c8`) sets flag
/// `+0x98` after the first seed (VA 0x339320), and only its constructor 0x338a1c clears it.
/// The app makes one seed at start-up and passes it to every `Session`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TrainerSeed(f32);

impl TrainerSeed {
    /// The first `mt19937` output for one `std::random_device` value. Stock passes the u32
    /// as a UInt QVariant; the float conversion on the param path is inferred.
    pub fn from_random_device(random: u32) -> Self {
        let mut mt = [0u32; 398];
        mt[0] = random;
        for i in 1..mt.len() {
            let previous = mt[i - 1];
            mt[i] = 1_812_433_253u32
                .wrapping_mul(previous ^ (previous >> 30))
                .wrapping_add(i as u32);
        }
        let y = (mt[0] & 0x8000_0000) | (mt[1] & 0x7fff_ffff);
        let mut x = mt[397] ^ (y >> 1) ^ if y & 1 == 1 { 0x9908_b0df } else { 0 };
        x ^= x >> 11;
        x ^= (x << 7) & 0x9d2c_5680;
        x ^= (x << 15) & 0xefc6_0000;
        x ^= x >> 18;
        Self(x as f32)
    }

    pub fn value(self) -> f32 {
        self.0
    }
}

/// Remote `save_info` (VA 0x1a4ce4) and the record fields the user supplies.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SaveInfo {
    pub name: String,
    pub device: i32,
    pub instrument: i32,
}

/// A new capture library record (VA 0x6fa0c4). The library adds the user name, account,
/// platform (`quad` or `quad_cortex_mini`) and UTC creation time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaptureRecord<'a> {
    pub info: &'a SaveInfo,
    /// Lowercase hex SHA-256 of `model` (VA 0x6a9204); the library key.
    pub hash: String,
    /// The raw exported model, without header or wrapper.
    pub model: &'a [u8],
}

/// Capture library collection 2 (VA 0x76be70): the user capture root folder ("My
/// Captures", product type 2, folder type 1) of `zenui.db`, made by get-or-create
/// `0x835204(db, 2, 1)` and found by folder key (`0x76bc68`, "Folder not found").
///
/// `insert` is the folder's add (vtable `+0x54`, from 0x6fa0c4): one product of type 2
/// whose id is `record.hash`, linked into that folder, with name, device, instrument,
/// user name and account (`0x30c080`; account empty when unset), platform
/// ([`Hardware::platform_name`]), UTC creation time and `record.model` as raw data.
/// Metadata and raw data are stored with the capture encryption (`nc_encryption`, key
/// injected). Stock completes `insert` through callbacks; this trait is synchronous.
pub trait CaptureLibrary {
    /// Duplicate check over both capture repositories (VA 0x6f5e20).
    fn contains(&mut self, hash: &str) -> io::Result<bool>;
    fn insert(&mut self, record: &CaptureRecord<'_>) -> io::Result<()>;
    /// Sets name, device and instrument of an existing record (VA 0x6f824c).
    fn update(&mut self, hash: &str, info: &SaveInfo) -> io::Result<()>;
}

/// Asynchronous inputs from the UI, the DSP response stream and services.
#[derive(Debug, Clone, PartialEq)]
pub enum Input {
    Command(Command),
    /// `dspBufferResponseSignal` (VA 0x33a774). Address 0 reports a failed allocation.
    BufferAllocated {
        id: BufferId,
        bytes: u32,
        address: u32,
    },
    /// `dspMeterResponseSignal`; `core` is the dialog's instance argument.
    Meter {
        core: u8,
        meter: u16,
        value: f32,
    },
    /// `dspModelStartResponseSignal` mapped to a capture node (VA 0x33a648): node 1 is the
    /// player, nodes 0 and 4 the conductor and node trainers.
    ModelStarted {
        node: u8,
        ok: bool,
    },
    /// The raw `dspModelStartResponseSignal(core, slot, ok)`. The session maps it to a
    /// row-0 node through the placement (VA 0x510be4); other sites are ignored.
    ModelStart {
        target: DeploymentTarget,
        ok: bool,
    },
    /// `DSPController::modelUmuteSignal` (VA 0x33a734).
    Unmuted,
    /// A deferred answer for an outstanding service gate.
    Fact(CallerFact),
    /// The result of a [`Entered::Pending`] plan for `state`.
    Entered {
        state: NcState,
        result: Result<Option<CapturePlacement>, ServiceError>,
    },
}

/// The DSPController reaction of one state, as its `EntryServices` gate sees it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Entry {
    Ready,
    Failed(ServiceError),
    /// The plan runs outside the session; `unmutes`: it holds an [`EntryAction::Unmute`].
    Pending {
        unmutes: bool,
    },
}

impl Entry {
    /// The gate reply, or `None` while the plan is pending.
    fn reply(self, state: NcState) -> Option<Reply> {
        match self {
            Self::Ready => Some(Reply::Fact(CallerFact::EntryServicesReady(state))),
            Self::Failed(ServiceError::Unsupported) => {
                Some(Reply::Failed(NcError::UnsupportedService))
            }
            Self::Failed(ServiceError::Failed) => Some(Reply::Failed(NcError::Backend)),
            Self::Pending { .. } => None,
        }
    }
}

/// Remote `NeuralCaptureMessage` (Cortex Control type 36) fields the controller sends.
#[derive(Debug, Clone, PartialEq)]
pub enum RemoteUpdate {
    /// Field 6 (VA 0x1a1e94).
    State(i32),
    /// Field 8, rounded and deduplicated (VA 0x1a2ba0).
    Progress(i32),
    /// Field 11, sanity bits on entry to 16 (VA 0x1a2518).
    ErrorId(i32),
    /// Field 7 entry; MicSetup input levels use index 1 and 6 (VA 0x1a13c4).
    Parameter { index: i32, value: f32 },
    /// Field 9 (VA 0x1a285c).
    ToggleAbModel(bool),
    /// Field 10, sent before the library write (VA 0x3d3f18).
    SaveInfo(SaveInfo),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Output {
    Notice(Notice),
    Remote(RemoteUpdate),
    /// Normalized MicSetup input level for UI channel 1 or 2.
    InputLevel {
        channel: u8,
        level: f32,
    },
    /// Conductor and node trainers both reported disengaged (VA 0x3cbbd8, meter 7).
    /// Stock then closes the capture (dialog vtable +0x68 = VA 0x3c9a04). The app must act:
    /// stop the meter timer, `SetNCOutSel(0,0,3,0,25000)` if it is on, close the dialog,
    /// then send `Command::Close` (controller state 0). The run stays in Training until then.
    TrainersDisengaged,
    /// The library holds the capture. `new`: inserted, and the run restarts at MicSetup.
    Saved {
        hash: String,
        new: bool,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    #[error("capture stimulus is unavailable ({0:?})")]
    Stimulus(StockError),
    #[error("capture orchestrator refused the event ({0:?})")]
    Refused(Refusal),
    #[error("no outstanding capture request matches this input")]
    Unmatched,
    #[error(transparent)]
    Dsp(#[from] DspError),
    #[error("capture library failed")]
    Library(#[source] io::Error),
    #[error("no capture preset placement is known yet")]
    NoPlacement,
    #[error("capture node {0} is not deployed (VA 0x510ba0)")]
    Unplaced(u8),
}

impl From<Refusal> for SessionError {
    fn from(refusal: Refusal) -> Self {
        Self::Refused(refusal)
    }
}

/// Meter requests of the dialog timer slot `meterRequestTimerSlot` (VA 0x3cbedc).
/// The caller's timer calls `Session::poll_meters` every `METER_POLL_MS`.
pub fn meter_poll(state: NcState) -> &'static [MeterRequest] {
    const fn at(core: u8, source: MeterSource, meter: u16) -> MeterRequest {
        MeterRequest {
            core,
            source,
            meter,
        }
    }
    const MIC: [MeterRequest; 2] = [
        at(0, MeterSource::Column(0), INPUT_LEVEL_2),
        at(0, MeterSource::Column(0), INPUT_LEVEL_1),
    ];
    const CAPTURE: [MeterRequest; 1] = [at(0, MeterSource::Node(1), 4)];
    const TRAINING: [MeterRequest; 4] = [
        at(0, MeterSource::Node(0), 5),
        at(0, MeterSource::Node(0), 6),
        at(0, MeterSource::Node(0), 7),
        at(1, MeterSource::Node(4), 7),
    ];
    const REFINE: [MeterRequest; 2] = [
        at(0, MeterSource::Node(1), 7),
        at(0, MeterSource::Node(1), 8),
    ];
    match state {
        NcState::MicSetup => &MIC,
        NcState::Calibrate | NcState::RefineCapture | NcState::Capturing => &CAPTURE,
        NcState::Training => &TRAINING,
        NcState::AutoRefine | NcState::RefineReady | NcState::ManualRefine | NcState::Exporting => {
            &REFINE
        }
        _ => &[],
    }
}

/// Default name for the next save (VA 0x3ce628). The token is the text after the last space
/// (the whole name without one). If it parses as an `i32` (`QString::toInt`), it is replaced
/// by `n + 1`; else `" 2"` is appended. An empty name stays empty.
///
/// Owned difference: stock chops the token and then appends `" " + (n + 1)`, so "Plexi 9"
/// becomes "Plexi  10" (two spaces) and "9" becomes " 10". This replaces the token in place.
pub fn next_capture_name(last: &str) -> String {
    if last.is_empty() {
        return String::new();
    }
    let stem = last.rfind(' ').map_or("", |space| &last[..=space]);
    match last[stem.len()..].parse::<i32>() {
        Ok(number) => format!("{stem}{}", number.wrapping_add(1)),
        Err(_) => format!("{last} 2"),
    }
}

/// `/dev/mem` window of one SHARC transfer (VA 0x333490, 0x337184).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DevMemWindow {
    /// Page-aligned physical base passed to `mmap`.
    pub base: u32,
    /// Offset of the buffer address inside the mapping.
    pub offset: u32,
    pub len: u32,
}

impl DevMemWindow {
    /// `base = addr & -page`, `len = (bytes - 1 + page + offset) & -page`.
    pub fn new(address: u32, bytes: u32, page: u32) -> Option<Self> {
        if !page.is_power_of_two() || bytes == 0 {
            return None;
        }
        let base = address & !(page - 1);
        let offset = address - base;
        let len = (bytes - 1).checked_add(page)?.checked_add(offset)? & !(page - 1);
        Some(Self { base, offset, len })
    }
}

/// MicSetup level law (VA 0x3cbbd8): clamp at -40 dB, then map -40..0 dB to 0..1.
pub fn input_level(db: f32) -> f32 {
    let db = if db <= -40.0 { -40.0 } else { db };
    (db + 40.0) / 40.0
}

/// Export counter meter 8: `(0 < v) * (int)v` (VA 0x3cbbd8).
fn export_count(value: f32) -> u32 {
    if value > 0.0 { value as i32 as u32 } else { 0 }
}

/// Stock relative change test before a remote parameter send (VA 0x1a13c4).
fn changed(last: Option<f32>, value: f32) -> bool {
    last.is_none_or(|last| (last - value).abs() > last.abs().min(value.abs()) * 1e-5)
}

/// Drives one `CaptureOrchestrator` with injected DSP, SHARC memory, storage and services.
///
/// Synchronous requests (transfers, parameter writes, frees) run at once. Allocations,
/// meter requests and deferred gates complete through `handle`. Remaining work stays
/// queued across calls, so an error return never drops a request.
pub struct Session<D, M, S, C> {
    machine: CaptureOrchestrator,
    paths: CapturePaths,
    dsp: D,
    memory: M,
    storage: S,
    services: C,
    seed: TrainerSeed,
    hardware: Hardware,
    controller: DspController,
    /// DSPController reaction per state; answers that state's `EntryServices` gate.
    entered: [Entry; 17],
    /// Stock mute latch `+0x18`: set on entry to 1 (not from 6), 11 and 12 (VA 0x338118),
    /// cleared by `modelUmuteSignal` (VA 0x33a734).
    muted: bool,
    placement: Option<CapturePlacement>,
    queue: VecDeque<Effect>,
    /// `QTimer::singleShot(0, exportModel)`: fires after the current queue drains.
    exports: VecDeque<RequestId>,
    allocations: Vec<(RequestId, BufferId)>,
    meters: Vec<(RequestId, u16)>,
    gates: Vec<(RequestId, Gate)>,
    poll_epoch: Option<Epoch>,
    /// Dialog progress value (`dialog[0x76]+0x118`, VA 0x3ca6d4); 0 on MicSetup entry.
    displayed: f32,
    /// Start acknowledgements of the conductor (node 0) and node (node 4) trainers.
    trainers: [bool; 2],
    last_error: Option<NcError>,
    remote_progress: Option<i32>,
    remote_levels: [Option<f32>; 2],
    disengaged: [bool; 2],
    outputs: Vec<Output>,
}

impl<D, M, S, C> Session<D, M, S, C>
where
    D: CaptureDsp,
    M: SharcMemory,
    S: CaptureStorage,
    C: CaptureServices,
{
    /// Reads both stimulus sizes as the stock constructor does (VA 0x338a1c). A missing
    /// stimulus fails here (owned); stock reports it only at transfer time.
    pub fn new(
        paths: CapturePaths,
        dsp: D,
        memory: M,
        mut storage: S,
        services: C,
        seed: TrainerSeed,
    ) -> Result<Self, SessionError> {
        let mut size = |path: &Path, error| {
            storage
                .file_len(path)
                .map_err(|_| SessionError::Stimulus(error))
                .map(|len| u32::try_from(len).unwrap_or(u32::MAX))
        };
        let sizes = Sizes {
            input_bytes: size(&paths.input_stimulus, StockError::InputStimulus)?,
            refine_bytes: size(&paths.refine_stimulus, StockError::RefineStimulus)?,
        };
        Ok(Self {
            machine: CaptureOrchestrator::new(sizes)?,
            paths,
            dsp,
            memory,
            storage,
            services,
            seed,
            hardware: Hardware::default(),
            controller: DspController::default(),
            entered: [Entry::Ready; 17],
            muted: false,
            placement: None,
            queue: VecDeque::new(),
            exports: VecDeque::new(),
            allocations: Vec::new(),
            meters: Vec::new(),
            gates: Vec::new(),
            poll_epoch: None,
            displayed: 0.0,
            trainers: [false; 2],
            last_error: None,
            remote_progress: None,
            remote_levels: [None; 2],
            disengaged: [false; 2],
            outputs: Vec::new(),
        })
    }

    /// Selects the hardware variant (stock `0x499a60`). The default is Quad Cortex.
    pub fn with_hardware(mut self, hardware: Hardware) -> Self {
        self.hardware = hardware;
        self
    }

    pub fn controller(&self) -> &DspController {
        &self.controller
    }
    pub fn machine(&self) -> &CaptureOrchestrator {
        &self.machine
    }
    pub fn paths(&self) -> &CapturePaths {
        &self.paths
    }
    pub fn dsp(&self) -> &D {
        &self.dsp
    }
    pub fn memory(&self) -> &M {
        &self.memory
    }
    pub fn storage(&self) -> &S {
        &self.storage
    }
    pub fn services(&self) -> &C {
        &self.services
    }

    pub fn handle(&mut self, input: Input) -> Result<Vec<Output>, SessionError> {
        match input {
            Input::Command(command) => self.apply(Event::Command(command))?,
            Input::BufferAllocated { id, bytes, address } => {
                let index = self
                    .allocations
                    .iter()
                    .position(|&(_, buffer)| buffer == id)
                    .ok_or(SessionError::Unmatched)?;
                let request = self.allocations[index].0;
                let reply = Reply::Allocated { id, bytes, address };
                self.apply(Event::Reply { request, reply })?;
                self.allocations.remove(index);
            }
            Input::Meter { core, meter, value } => self.meter(core, meter, value)?,
            Input::ModelStarted { node, ok } => self.model_started(node, ok)?,
            Input::ModelStart { target, ok } => {
                let placement = self.placement.as_ref().ok_or(SessionError::NoPlacement)?;
                // Stock maps unknown targets to (-1, -1), which match no node.
                if let Some((0, node)) = placement.site(target) {
                    self.model_started(node, ok)?;
                }
            }
            Input::Unmuted => {
                if self.machine.state() != NcState::Inactive {
                    self.muted = false;
                }
                if let Some(index) = self.gates.iter().position(|&(_, g)| g == Gate::Unmuted) {
                    let request = self.gates[index].0;
                    self.apply(Event::Reply {
                        request,
                        reply: Reply::Fact(CallerFact::Unmuted),
                    })?;
                    self.gates.remove(index);
                }
            }
            Input::Entered { state, result } => self.entered(state, result)?,
            Input::Fact(fact) => {
                let index = self
                    .gates
                    .iter()
                    .position(|&(_, gate)| answers(gate, &fact))
                    .ok_or(SessionError::Unmatched)?;
                let request = self.gates[index].0;
                self.apply(Event::Reply {
                    request,
                    reply: Reply::Fact(fact),
                })?;
                self.gates.remove(index);
            }
        }
        self.pump()?;
        Ok(std::mem::take(&mut self.outputs))
    }

    /// Sends the current state's dialog meter requests. Replies are bound to this entry.
    pub fn poll_meters(&mut self) -> Result<(), SessionError> {
        let requests = meter_poll(self.machine.state())
            .iter()
            .map(|&request| self.resolve(request))
            .collect::<Result<Vec<_>, _>>()?;
        self.poll_epoch = Some(self.machine.epoch());
        for request in requests {
            self.dsp.request_meter(request)?;
        }
        Ok(())
    }

    /// Node sources become DSP slots of the current placement (VA 0x510ba0).
    fn resolve(&self, request: MeterRequest) -> Result<MeterRequest, SessionError> {
        let (Some(placement), MeterSource::Node(node)) = (&self.placement, request.source) else {
            return Ok(request);
        };
        let target = placement
            .node_target(node)
            .ok_or(SessionError::Unplaced(node))?;
        Ok(MeterRequest {
            source: MeterSource::Column(target.slot.index()),
            ..request
        })
    }

    /// Save flow from RefineReady (VA 0x3ce92c, 0x3d3f18). A new hash is inserted and the
    /// run restarts at MicSetup; a known hash only updates its name, device and instrument.
    pub fn save(
        &mut self,
        info: SaveInfo,
        library: &mut (impl CaptureLibrary + ?Sized),
    ) -> Result<Vec<Output>, SessionError> {
        if self.machine.state() != NcState::RefineReady {
            return Err(Refusal::Unsupported.into());
        }
        if self.machine.pending_requests().next().is_some() {
            return Err(Refusal::Busy.into());
        }
        let model = self.machine.model().ok_or(Refusal::Unsupported)?;
        let hash: String = Sha256::digest(model)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        let new = !library.contains(&hash).map_err(SessionError::Library)?;
        if new {
            let record = CaptureRecord {
                info: &info,
                hash: hash.clone(),
                model,
            };
            library.insert(&record).map_err(SessionError::Library)?;
            self.apply(Event::Command(Command::Saved))?;
        } else {
            library
                .update(&hash, &info)
                .map_err(SessionError::Library)?;
        }
        // Stock sends save_info before the library write; a failed save emits nothing.
        self.outputs
            .push(Output::Remote(RemoteUpdate::SaveInfo(info)));
        self.outputs.push(Output::Saved { hash, new });
        self.pump()?;
        Ok(std::mem::take(&mut self.outputs))
    }

    /// Fails delay/sanity meter requests the DSP never answered (caller timeout).
    pub fn expire_meters(&mut self) -> Result<Vec<Output>, SessionError> {
        while let Some(&(request, _)) = self.meters.first() {
            self.apply(Event::Reply {
                request,
                reply: Reply::Failed(NcError::Backend),
            })?;
            self.meters.remove(0);
        }
        self.pump()?;
        Ok(std::mem::take(&mut self.outputs))
    }

    fn apply(&mut self, event: Event) -> Result<(), SessionError> {
        let effects = self.machine.apply(event)?;
        self.queue.extend(effects);
        Ok(())
    }

    /// Runs the queue. A held gate that a cancellation abandoned gets its matching fact,
    /// which the machine only drains: its model start, unmute or entry no longer answers it,
    /// and cleanup waits for every abandoned request.
    fn pump(&mut self) -> Result<(), SessionError> {
        loop {
            if let Some(effect) = self.queue.pop_front() {
                self.execute(effect)?;
            } else if let Some(index) = self
                .gates
                .iter()
                .position(|&(request, _)| self.machine.abandoned(request))
            {
                let (request, gate) = self.gates.remove(index);
                let fact = match gate {
                    Gate::EntryServices(state) => CallerFact::EntryServicesReady(state),
                    Gate::PlayerStarted => CallerFact::PlayerStarted,
                    Gate::TrainersStarted => CallerFact::TrainersStarted {
                        conductor: true,
                        node: true,
                    },
                    Gate::Unmuted => CallerFact::Unmuted,
                    Gate::DialogProgress => CallerFact::DialogProgress { minimum: 0.0 },
                    Gate::TrainerInputs => CallerFact::TrainerInputsReady { seed: 0.0 },
                    Gate::DiscardDecision => CallerFact::DiscardAllowed(false),
                    Gate::Quiesce => CallerFact::BackendQuiesced,
                };
                self.apply(Event::Reply {
                    request,
                    reply: Reply::Fact(fact),
                })?;
            } else if let Some(&request) = self.exports.front() {
                self.apply(Event::Reply {
                    request,
                    reply: Reply::ExportDue,
                })?;
                self.exports.pop_front();
            } else {
                return Ok(());
            }
        }
    }

    fn execute(&mut self, effect: Effect) -> Result<(), SessionError> {
        match effect {
            Effect::Notify(notice) => self.notify(notice),
            // Only offered work is withdrawn, and this executor offers nothing beyond its queue.
            Effect::Withdraw(id) => {
                let queued = self.queue.iter().position(
                    |effect| matches!(effect, Effect::Request { id: queued, .. } if *queued == id),
                );
                if let Some(index) = queued {
                    self.queue.remove(index);
                    self.apply(Event::NotSent(id))?;
                }
            }
            Effect::Request { id, operation, .. } => self.request(id, operation)?,
        }
        Ok(())
    }

    fn request(&mut self, id: RequestId, operation: Operation) -> Result<(), SessionError> {
        let reply = match operation {
            Operation::Allocate { id: buffer, bytes } => match self.dsp.allocate(buffer, bytes) {
                Ok(()) => {
                    self.apply(Event::Sent(id))?;
                    self.allocations.push((id, buffer));
                    return Ok(());
                }
                Err(DspError::NotDelivered) => return self.apply(Event::NotSent(id)),
                Err(DspError::Failed) => Reply::Failed(NcError::Backend),
            },
            Operation::RequestMeter { id: meter } => {
                // Delay and sanity requests read node 1 (VA 0x339320, 0x333050).
                let request = self.resolve(MeterRequest {
                    core: 0,
                    source: MeterSource::Node(1),
                    meter: u16::from(meter),
                });
                let Ok(request) = request else {
                    self.apply(Event::Sent(id))?;
                    return self.apply(Event::Reply {
                        request: id,
                        reply: Reply::Failed(NcError::UnsupportedService),
                    });
                };
                match self.dsp.request_meter(request) {
                    Ok(()) => {
                        self.apply(Event::Sent(id))?;
                        self.meters.push((id, request.meter));
                        return Ok(());
                    }
                    Err(DspError::NotDelivered) => return self.apply(Event::NotSent(id)),
                    Err(DspError::Failed) => Reply::Failed(NcError::Backend),
                }
            }
            Operation::ScheduleExport => {
                self.apply(Event::Sent(id))?;
                self.exports.push_back(id);
                return Ok(());
            }
            Operation::AwaitCaller(Gate::TrainerInputs) => {
                Reply::Fact(CallerFact::TrainerInputsReady {
                    seed: self.seed.value(),
                })
            }
            Operation::AwaitCaller(Gate::DialogProgress) => {
                Reply::Fact(CallerFact::DialogProgress {
                    minimum: self.displayed,
                })
            }
            Operation::AwaitCaller(gate @ Gate::EntryServices(state)) => {
                match self.entered[state as usize].reply(state) {
                    Some(reply) => reply,
                    None => {
                        self.apply(Event::Sent(id))?;
                        self.gates.push((id, gate));
                        return Ok(());
                    }
                }
            }
            Operation::AwaitCaller(Gate::Unmuted) if !self.muted => {
                Reply::Fact(CallerFact::Unmuted)
            }
            Operation::AwaitCaller(
                gate @ (Gate::PlayerStarted | Gate::TrainersStarted | Gate::Unmuted),
            ) => {
                if gate == Gate::TrainersStarted {
                    self.trainers = [false; 2];
                }
                self.apply(Event::Sent(id))?;
                self.gates.push((id, gate));
                return Ok(());
            }
            // Back asks "Unsaved capture" only while no exported model is held (VA 0x3cea8c).
            Operation::AwaitCaller(Gate::DiscardDecision) if self.machine.model().is_some() => {
                Reply::Fact(CallerFact::DiscardAllowed(true))
            }
            Operation::AwaitCaller(gate) => match self.services.request(gate) {
                Ok(Some(fact)) => Reply::Fact(fact),
                Ok(None) => {
                    self.apply(Event::Sent(id))?;
                    self.gates.push((id, gate));
                    return Ok(());
                }
                Err(ServiceError::Unsupported) => Reply::Failed(NcError::UnsupportedService),
                Err(ServiceError::Failed) => Reply::Failed(NcError::Backend),
            },
            Operation::FreeBuffer(lease) => match self.dsp.free(lease.id) {
                Ok(()) => Reply::Freed(lease),
                Err(DspError::NotDelivered) => return self.apply(Event::NotSent(id)),
                Err(DspError::Failed) => Reply::Failed(NcError::Backend),
            },
            Operation::SetParameters(writes) => {
                // Only the state-1 setup writes run in 1 and 6; mixes precede them (VA 0x339320).
                let mixes = match self.machine.state() {
                    NcState::Initial | NcState::MicSetup => &self.hardware.start_mixes()[..],
                    _ => &[],
                };
                match self.set_parameters(mixes, &writes) {
                    Some(reply) => reply,
                    None => return self.apply(Event::NotSent(id)),
                }
            }
            Operation::WriteFile { to, file, bytes } => self.file_to_sharc(to, file, bytes),
            Operation::ReadFile { from, file, bytes } => self.sharc_to_file(from, file, bytes),
            Operation::ReadModel { from, bytes } => {
                let mut data = vec![0; bytes as usize];
                match self.memory.read(from.address, &mut data) {
                    Ok(()) => Reply::ModelRead {
                        lease: from,
                        bytes: data,
                    },
                    Err(_) => Reply::Failed(NcError::Stock(StockError::MemoryRead)),
                }
            }
            // refreshModel (VA 0x3332ac) copies the in-memory export back to buffer 5.
            Operation::WriteModel { to, bytes } => match self.memory.write(to.address, &bytes) {
                Ok(()) => Reply::TransferDone {
                    lease: to,
                    bytes: bytes.len() as u32,
                },
                Err(_) => Reply::Failed(NcError::Stock(StockError::MemoryWrite)),
            },
        };
        self.apply(Event::Sent(id))?;
        self.apply(Event::Reply { request: id, reply })
    }

    /// `None` means nothing was delivered. A later failure may follow an applied prefix.
    fn set_parameters(&mut self, mixes: &[(u8, f32)], writes: &[ParamWrite]) -> Option<Reply> {
        let sends = mixes.iter().map(Ok).chain(writes.iter().map(Err));
        for (index, send) in sends.enumerate() {
            let result = match (send, &self.placement) {
                (Ok(&(mix, level)), _) => self.dsp.set_headphone_mix(mix, level),
                (Err(&write), None) => self.dsp.set_parameter(write),
                (Err(&write), Some(placement)) => match placement.node_target(write.node) {
                    Some(target) => self.dsp.set_placed_parameter(target, write),
                    None => return Some(Reply::Failed(NcError::UnsupportedService)),
                },
            };
            match result {
                Ok(()) => {}
                Err(DspError::NotDelivered) if index == 0 => return None,
                Err(_) => return Some(Reply::Failed(NcError::Backend)),
            }
        }
        Some(Reply::Executed)
    }

    /// VA 0x333490: copy `bytes` from the start of a file to the buffer address.
    fn file_to_sharc(&mut self, to: BufferLease, file: FileRole, bytes: u32) -> Reply {
        let mut data = vec![0; bytes as usize];
        if self.storage.read(self.paths.get(file), &mut data).is_err() {
            return Reply::Failed(NcError::Stock(StockError::InputStimulus));
        }
        if self.memory.write(to.address, &data).is_err() {
            return Reply::Failed(NcError::Stock(StockError::MemoryWrite));
        }
        Reply::TransferDone { lease: to, bytes }
    }

    /// VA 0x337758: copy `bytes` from the buffer address to a truncated, synced file.
    fn sharc_to_file(&mut self, from: BufferLease, file: FileRole, bytes: u32) -> Reply {
        let mut data = vec![0; bytes as usize];
        if self.memory.read(from.address, &mut data).is_err() {
            return Reply::Failed(NcError::Stock(StockError::MemoryRead));
        }
        if self.storage.write(self.paths.get(file), &data).is_err() {
            return Reply::Failed(NcError::Stock(StockError::OutputFile));
        }
        Reply::TransferDone { lease: from, bytes }
    }

    /// A failed start fails the run (owned; stock waits for another acknowledgement).
    fn model_started(&mut self, node: u8, ok: bool) -> Result<(), SessionError> {
        let waiting = self.gates.iter().position(|&(_, gate)| match gate {
            Gate::PlayerStarted => node == 1,
            Gate::TrainersStarted => node == 0 || node == 4,
            _ => false,
        });
        let Some(index) = waiting else {
            return Ok(());
        };
        let (request, gate) = self.gates[index];
        let reply = if !ok {
            Reply::Failed(NcError::Backend)
        } else if gate == Gate::PlayerStarted {
            Reply::Fact(CallerFact::PlayerStarted)
        } else {
            self.trainers[usize::from(node == 4)] = true;
            if self.trainers != [true; 2] {
                return Ok(());
            }
            Reply::Fact(CallerFact::TrainersStarted {
                conductor: true,
                node: true,
            })
        };
        self.apply(Event::Reply { request, reply })?;
        self.gates.remove(index);
        Ok(())
    }

    fn meter(&mut self, core: u8, meter: u16, value: f32) -> Result<(), SessionError> {
        // A requested delay or sanity meter answers its request (VA 0x33a9c8, 0x33aa10).
        let requested = self.meters.iter().position(|&(_, m)| m == meter);
        if let Some(index) = requested
            && core == 0
        {
            let request = self.meters[index].0;
            let reply = match meter {
                _ if !value.is_finite() => Reply::Failed(NcError::NonFinite),
                5 => Reply::BulkDelay(value),
                // Stock casts to u8; negative values read as 0, like ARM `vcvt`.
                _ => Reply::Sanity(value as u8),
            };
            self.apply(Event::Reply { request, reply })?;
            self.meters.remove(index);
            return Ok(());
        }
        let Some(epoch) = self.poll_epoch else {
            return Ok(());
        };
        let state = self.machine.state();
        let event = match (state, meter) {
            (NcState::MicSetup, INPUT_LEVEL_1 | INPUT_LEVEL_2) => {
                self.level(meter, input_level(value));
                return Ok(());
            }
            // The dialog drops capture progress below the displayed value (VA 0x3cbbd8).
            (NcState::Calibrate | NcState::RefineCapture | NcState::Capturing, 4)
                if value < self.displayed =>
            {
                return Ok(());
            }
            (NcState::Calibrate | NcState::RefineCapture | NcState::Capturing, 4)
            | (NcState::Training, 5)
            | (
                NcState::AutoRefine
                | NcState::RefineReady
                | NcState::ManualRefine
                | NcState::Exporting,
                7,
            ) => Event::Progress { epoch, value },
            (NcState::Training, 6) => Event::Loss { epoch, value },
            (NcState::Training, 7) => {
                self.disengage(core, value);
                return Ok(());
            }
            (
                NcState::AutoRefine
                | NcState::RefineReady
                | NcState::ManualRefine
                | NcState::Exporting,
                8,
            ) => Event::ExportCounter {
                epoch,
                value: export_count(value),
            },
            _ => return Ok(()),
        };
        match self.machine.apply(event) {
            Ok(effects) => self.queue.extend(effects),
            // Meters from another entry, stage or state are ignored, as in stock.
            Err(Refusal::Stale | Refusal::Busy | Refusal::Unsupported | Refusal::NonFinite) => {}
            Err(refusal) => return Err(refusal.into()),
        }
        Ok(())
    }

    fn level(&mut self, meter: u16, level: f32) {
        let (channel, index) = if meter == INPUT_LEVEL_1 {
            (1, 1)
        } else {
            (2, 6)
        };
        self.outputs.push(Output::InputLevel { channel, level });
        let last = &mut self.remote_levels[usize::from(channel - 1)];
        if changed(*last, level) {
            *last = Some(level);
            self.outputs.push(Output::Remote(RemoteUpdate::Parameter {
                index,
                value: level,
            }));
        }
    }

    fn disengage(&mut self, core: u8, value: f32) {
        if let Some(flag) = self.disengaged.get_mut(usize::from(core))
            && value >= 0.5
        {
            *flag = true;
        }
        if self.disengaged == [true; 2] {
            self.disengaged = [false; 2];
            self.outputs.push(Output::TrainersDisengaged);
        }
    }

    fn notify(&mut self, notice: Notice) {
        match notice {
            Notice::State(state) => {
                self.enter_services(state);
                self.outputs
                    .push(Output::Remote(RemoteUpdate::State(state as i32)));
                if state == NcState::HandleError
                    && let Some(NcError::Sanity(bits)) = self.last_error
                {
                    self.outputs
                        .push(Output::Remote(RemoteUpdate::ErrorId(i32::from(bits))));
                }
                if state != NcState::Training {
                    self.disengaged = [false; 2];
                }
                if state == NcState::MicSetup {
                    self.displayed = 0.0;
                }
            }
            Notice::Progress(value) => {
                self.displayed = value;
                let rounded = value.round() as i32;
                if self.remote_progress != Some(rounded) {
                    self.remote_progress = Some(rounded);
                    self.outputs
                        .push(Output::Remote(RemoteUpdate::Progress(rounded)));
                }
            }
            Notice::Error(error) => self.last_error = Some(error),
            Notice::AbModel(model) => self
                .outputs
                .push(Output::Remote(RemoteUpdate::ToggleAbModel(model))),
            Notice::CorruptCapture => {}
        }
        self.outputs.push(Output::Notice(notice));
    }
}

impl<D, M, S, C> Session<D, M, S, C>
where
    D: CaptureDsp,
    M: SharcMemory,
    S: CaptureStorage,
    C: CaptureServices,
{
    /// `setNcState` (VA 0x338118) sets the mute latch, then `ncStateChanged` runs the
    /// DSPController reaction synchronously (VA 0x528498).
    fn enter_services(&mut self, state: NcState) {
        let from = self.controller.last();
        if matches!(state, NcState::Training | NcState::AutoRefine)
            || (state == NcState::Initial && from != NcState::MicSetup)
        {
            self.muted = true;
        }
        let plan = self.controller.enter(state);
        if plan.is_empty() {
            self.entered[state as usize] = Entry::Ready;
            return;
        }
        let unmutes = plan
            .iter()
            .any(|action| matches!(action, EntryAction::Unmute { .. }));
        self.entered[state as usize] = match self.services.enter(state, &plan) {
            Ok(Entered::Done(placement)) => {
                self.finish_entry(placement, unmutes);
                Entry::Ready
            }
            Ok(Entered::Pending) => Entry::Pending { unmutes },
            Err(error) => Entry::Failed(error),
        };
    }

    /// [`Input::Entered`]: settles the pending plan of `state` and answers its gate when
    /// the gate already waits.
    fn entered(
        &mut self,
        state: NcState,
        result: Result<Option<CapturePlacement>, ServiceError>,
    ) -> Result<(), SessionError> {
        let Entry::Pending { unmutes } = self.entered[state as usize] else {
            return Err(SessionError::Unmatched);
        };
        let entry = match result {
            Ok(placement) => {
                self.finish_entry(placement, unmutes);
                Entry::Ready
            }
            Err(error) => Entry::Failed(error),
        };
        self.entered[state as usize] = entry;
        let gate = Gate::EntryServices(state);
        if let Some(index) = self.gates.iter().position(|&(_, waiting)| waiting == gate) {
            let (request, _) = self.gates.remove(index);
            let reply = entry.reply(state).expect("settled above");
            self.apply(Event::Reply { request, reply })?;
        }
        Ok(())
    }

    fn finish_entry(&mut self, placement: Option<CapturePlacement>, unmutes: bool) {
        if placement.is_some() {
            self.placement = placement;
        }
        if unmutes {
            self.muted = false;
        }
    }
}

/// The same gate/fact pairing the orchestrator validates.
fn answers(gate: Gate, fact: &CallerFact) -> bool {
    match (gate, fact) {
        (Gate::EntryServices(state), CallerFact::EntryServicesReady(actual)) => state == *actual,
        (Gate::PlayerStarted, CallerFact::PlayerStarted)
        | (Gate::TrainersStarted, CallerFact::TrainersStarted { .. })
        | (Gate::Unmuted, CallerFact::Unmuted)
        | (Gate::DialogProgress, CallerFact::DialogProgress { .. })
        | (Gate::TrainerInputs, CallerFact::TrainerInputsReady { .. })
        | (Gate::DiscardDecision, CallerFact::DiscardAllowed(_))
        | (Gate::Quiesce, CallerFact::BackendQuiesced) => true,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dev_mem_window_matches_stock_page_math() {
        let window = DevMemWindow::new(0x2000_1234, 0x2864, 0x1000).unwrap();
        assert_eq!(
            window,
            DevMemWindow {
                base: 0x2000_1000,
                offset: 0x234,
                len: 0x3000
            }
        );
        assert_eq!(
            DevMemWindow::new(0x1000, 0x1000, 0x1000).unwrap().len,
            0x1000
        );
        assert_eq!(DevMemWindow::new(0x1000, 0, 0x1000), None);
    }

    #[test]
    fn next_capture_name_follows_the_stock_token_rule() {
        // Vectors from VA 0x3ce628; "Plexi 9" keeps one space (stock: two).
        for (last, next) in [
            ("Plexi 9", "Plexi 10"),
            ("Plexi", "Plexi 2"),
            ("Plexi9", "Plexi9 2"),
            ("Amp v2", "Amp v2 2"),
            ("Amp -3", "Amp -2"),
            ("", ""),
        ] {
            assert_eq!(next_capture_name(last), next, "{last:?}");
        }
    }
}

//! DSP bridge. SOC1 boot through tetrad-loader, the stock session from tetrad-dsp-host, the SOC2
//! slave boot, boot keys, global audio settings, preset deploys with the stock per-model setup
//! (`tetrad_dsp_host::setup`), live edits and scene recall, with the shared blob loads, the
//! sidechain step and the table amps.
//!
//! Every grid block keeps its setup [`Block`] for later edits and recalls. A block whose setup
//! is refused (driver not recovered, capture, missing stock table) is held in a muted bypass
//! (`0x0c` 2.0) and reported in [`Pending`]: it never runs with unset values (owned policy).
//!
//! SOC2 failure contract (a-boot §6, `0x89c0bc`, `0x89b7d4`, `0x89bdec`, `0x89bae0`), enforced by
//! `DspSession::boot` plus `Soc2Hooks`:
//! - status failure with no retry left: protocol cores 2, 3 and SOC2 are dead;
//! - a retry `0x12` that fails to send: all five protocol cores are dead, SOC1 included;
//! - an image copy failure: nothing is marked dead; stock stops silently, this boot returns
//!   [`Soc2Status::Stalled`] and still pulls the keys (owned choice);
//! - `0x14` carries argument 0 (stock: stale stack bytes).

use std::collections::{BTreeMap, HashMap};
use std::time::Duration;

use qc_format::blob::{ModuleBlob, Section};
use qc_format::diff::DiffFile;
use tetrad_catalog::{ModelSpec, ParameterType, Specs, ids};
use tetrad_control::codecs::{ControlMessage, GridMessage, MessageAction, RawMessageBytes};
use tetrad_core::performance::{BypassValue, RecallBlock, RecallStep, recall_plan};
use tetrad_core::workflow::BlockAddress;
use tetrad_dsp_host::blob::{self, BlobKey};
use tetrad_dsp_host::model::CoreWindows;
use tetrad_dsp_host::responses::{CrashCause, Event as DspEvent, KeyMaterial};
use tetrad_dsp_host::session::{
    BlobLoad, CORE0_SILENT, Clock, CoreLoad, DspSession, RunSlot, SessionError, SlaveBootOutcome,
    Soc2ImageLengths,
};
use tetrad_dsp_host::setup::drivers::lane_io::{Lane, LaneIo, LaneRouter, Tuner};
use tetrad_dsp_host::setup::drivers::looper::{
    Looper, LooperContext, LooperPlayback, LooperProduct, LooperTables, LooperWrite, Placements,
};
use tetrad_dsp_host::setup::drivers::media::{CabIrs, CaptureData};
use tetrad_dsp_host::setup::drivers::preblock::{
    self, Junction, JunctionSite, RouterSetup, push_routers,
};
use tetrad_dsp_host::setup::drivers::sidechain::{
    self, GridCell, PairJunction, RouteMode, SidechainContext, SidechainSink, Sidechains,
};
use tetrad_dsp_host::setup::{
    Block, BlockBypass, CatalogParameter, DriverMidi, DriverTables, DspWrite, Factory, LinkedEdit,
    MediaLibrary, ModelIdentity, ParamSetMode, SetupValue, TableError,
};
use tetrad_dsp_host::transport::{ControlPacket, DspTransport};
use tetrad_dsp_protocol::controls::ControlRequest;
use tetrad_dsp_protocol::packet::BulkMode;
use tetrad_dsp_protocol::requests::{BypassState, Request, SendPolicy};
use tetrad_dsp_protocol::{CoreId, DeploymentTarget, DspCore, ModuleSlot};
use tetrad_engine::{
    Deployed, Deployment, DeploymentPlan, Endpoint, GateMessage, Instance, Layout, Node,
    OutputPort, Pair, Placement, PlanOp, ROWS, RouterValue, RouterWrite, Row, SceneIndex,
    SidechainPlan, SidechainSource, Site, Slot, Update,
};
use tetrad_loader::booter::StockBooter;
use tetrad_loader::exec::{CoreFailure, execute, execute_soc1};
use tetrad_loader::plan::{Start, Variant, plan_soc1};
use tetrad_loader::soc2::{SETTLE, Soc2Boot, Soc2Images};
use tetrad_loader::{Limits, PhysAddr, RawLdr};
use tetrad_media::ir::IrSamples;
use tetrad_settings::device::Settings;
use tetrad_settings::general::GlobalBypassRows;

use crate::tempo::{ResendPlan, ResendStep};

use super::community::CommunityBlobs;
use super::owned::{InstallError, OWNED_CORE, OwnedBlobs, OwnedLoad};
use super::platform::{DspFile, Marker, Platform, Soc2Pair};
use super::services::{Licences, PluginStatus};
use qcdsp::device::VolumeTemplate;

/// Stock pause between the deploy mute and the run-list clear (c-dsp §6, `0x585464`).
pub const DEPLOY_SETTLE: Duration = Duration::from_millis(20);
/// The caller flag of `0x585464` (`param_4`); 0 is inferred.
const DEPLOY_FLAG: u32 = 0;
/// `0x876470(dsp, 1, flag)` at the top of `0x585464`. Bit 0 set mutes: inferred from this site
/// and standby `(1, 1)`; bring-up stage S4a proves it. Under this reading the power path's
/// value 0 (tetrad-watchdog `PowerFlags(0)`, named "mute first" there) is an unmute.
pub const DEPLOY_MUTE: u32 = 1 | DEPLOY_FLAG << 1;
/// Unmute after a deploy. Stock unmutes from a timer seeded at `+0x3400`; value 0 is inferred.
pub const UNMUTED: u32 = 0;

const DSP_CORES: [DspCore; 4] = [
    DspCore::Core0,
    DspCore::Core1,
    DspCore::Core2,
    DspCore::Core3,
];
/// Categories the global cab bypass rows force (`0x41111c`); the IR rows force category 29.
const CAB_CATEGORIES: [u32; 4] = [12, 21, 32, 33];
const IR_LOADER_CATEGORY: u32 = 29;
/// Model 27000 sends parameter 12 last when it is 0 (`0x527248`).
const LOOPER_MODEL: u32 = 27_000;
const LOOPER_LATE_PARAMETER: usize = 12;
/// The FX loop whose send is entry 1 of compiler table `+0x624` (`0x581890`).
const FX_LOOP_SECOND_SEND: u32 = 13_005;
/// Drivers start at 120 BPM until the first tempo change.
const DEFAULT_BPM: i32 = 120;

/// How a push started: a run-list load sends the compiler router setup and enables the
/// gate; a same-layout push does neither (tetrad-engine `Update::Reuse`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PushKind {
    Redeploy,
    Reuse,
}

#[derive(Debug, thiserror::Error)]
pub enum DspError {
    #[error("device feature: {0}")]
    Feature(String),
    #[error("not supported yet: {0}")]
    Unsupported(&'static str),
    #[error("platform: {0}")]
    Platform(#[source] Box<dyn std::error::Error + Send + Sync>),
    #[error(transparent)]
    Image(#[from] tetrad_loader::Error),
    #[error(transparent)]
    Soc1(#[from] tetrad_loader::exec::Soc1Error),
    #[error(transparent)]
    Transport(#[from] tetrad_dsp_host::transport::TransportError),
    #[error(transparent)]
    Session(#[from] SessionError),
    /// FitChecker refused a core (`0x879100`); nothing was loaded.
    #[error("requested models won't fit in {core:?} ({section:?} overflows); nothing deployed")]
    WontFit { core: DspCore, section: Section },
    #[error(transparent)]
    Protocol(#[from] tetrad_dsp_protocol::Error),
    #[error(transparent)]
    Blob(#[from] tetrad_dsp_host::blob::BlobError),
    #[error(transparent)]
    Format(#[from] qc_format::FormatError),
    #[error("stock driver tables: {0}")]
    Tables(#[from] TableError),
    #[error(transparent)]
    Route(#[from] tetrad_engine::RouteError),
    #[error("the DSP did not send the {0} key")]
    MissingKey(&'static str),
    /// The opt-in owned stock-ID map was refused; every model stays stock.
    #[error(transparent)]
    Owned(#[from] InstallError),
    /// A deploy failed after the run-list clear (`0x1e`): the DSP no longer runs the
    /// committed plan. Edits and recalls are refused until a deploy succeeds.
    #[error("the DSP does not run the deployed preset (a deploy failed); load a preset again")]
    OutOfSync,
    /// A Neural Capture preset runs (DSPController `+0x88d0`): the user preset's placements
    /// belong to another run list. Owned: stock points these edits at the capture preset.
    #[error("a Neural Capture preset runs; the user preset is not deployed")]
    CapturePreset,
}

impl DspError {
    pub(crate) fn platform(error: impl std::error::Error + Send + Sync + 'static) -> Self {
        Self::Platform(Box::new(error))
    }
}

/// How the SOC2 slave boot ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Soc2Status {
    Open,
    /// `/opt/neuraldsp/noslaveboot`: handshake and the SOC2 open (GPIO 97 gate) only.
    Skipped,
    /// Booted, but the platform could not open SOC2 comms.
    CommsFailed,
    /// The DSP reported failure; cores 2, 3 and SOC2 are dead.
    Dead,
    /// The image copy failed; stock stops silently and marks nothing dead.
    Stalled,
}

impl Soc2Status {
    /// SOC2 cores the boot leaves without a link: stock `0x89b2f0` then fails, so the boot
    /// end (`0x545ac0` → `0x513a70`) shows the DSPErrorDialog. `CommsFailed` is inferred.
    pub fn faulted(self) -> bool {
        matches!(self, Self::CommsFailed | Self::Dead)
    }
}

/// The stock DSP crash latch (DSPController `+0x89e8`, never cleared before the reboot) and
/// the DSPErrorDialog ("Something went wrong", CANCEL and REBOOT, ctor `0x6c6248`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DspFault {
    /// The first unhealthy sign; later ones do not replace it.
    pub cause: FaultCause,
    /// The dialog shows. CANCEL (`0x514c48`) and REBOOT (`0x514a6c`) hide it.
    pub dialog: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FaultCause {
    /// `dspCrashSignal` (`0x9ae428`): four send timeouts or a run-list timeout (crash handler
    /// `0x89c680`), or a module start failure (drain `0x89ece0`). Slot `0x5149dc` shows the
    /// dialog again on each signal.
    Crash { core: CoreId, cause: CrashCause },
    /// Non-zero diagnostics error flags (`+0x3c4..+0x3d0`, `0x8a136c`) fail the health test
    /// `0x89b2f0`; the timer gate `0x51399c` then shows the dialog once.
    ErrorFlags { core: DspCore, flags: u32 },
    /// A dead SOC2 at the boot end ([`Soc2Status::faulted`]).
    Boot(Soc2Status),
}

impl DspFault {
    /// Latches `cause` and shows the dialog: always for a crash signal (`0x5149dc` has no
    /// latch test), else only the first time (`0x51399c`, `0x513a70`).
    pub fn raise(fault: &mut Option<Self>, cause: FaultCause) {
        match fault {
            Some(latched) => latched.dialog |= matches!(cause, FaultCause::Crash { .. }),
            None => {
                *fault = Some(Self {
                    cause,
                    dialog: true,
                })
            }
        }
    }
}

/// [`DspFault::raise`], then the fault mute ([`Dsp::latch_fault_mute`]) on every raise, so
/// a second crash mutes again. Without a DSP only the latch changes.
pub fn raise_fault<P: Platform>(
    dsp: Option<&mut Dsp<P>>,
    fault: &mut Option<DspFault>,
    cause: FaultCause,
) -> Result<(), DspError> {
    DspFault::raise(fault, cause);
    dsp.map_or(Ok(()), Dsp::latch_fault_mute)
}

/// Steps a running QC needs that were not done. The caller reports each one.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Pending(pub Vec<String>);

impl Pending {
    fn note(&mut self, step: impl Into<String>) {
        let step = step.into();
        if !self.0.contains(&step) {
            self.0.push(step);
        }
    }
}

/// What a value push reads besides the update: the catalog, the committed deployment (layout,
/// plan and selected scene) and the global cab/IR bypass rows.
#[derive(Clone, Copy)]
pub struct Context<'a> {
    pub specs: &'a Specs,
    pub deployment: &'a Deployment,
    pub global_bypass: GlobalBypassRows,
    /// Tuner engine flag (controller `+0x88d8`): on from boot, off only on the Neural
    /// Capture screen. `State.tuner_engine` feeds it, never `State::screen`.
    /// While on, the lane I/O push ends with the tuner path.
    pub tuner_active: bool,
    /// The tuner input port (controller `+0x88dc`, from the tuner settings).
    pub tuner_input: u32,
    /// The tuner mutes the lanes that read its port (controller `+0x88e0`): set while the
    /// tuner is shown with its mute setting, or by the live tuner.
    pub tuner_muted: bool,
    /// IRs and captures the plan names, resolved by the device; `None`: nothing resolved.
    pub media: Option<&'a ResolvedMedia>,
    pub expression: Option<&'a super::features::Expression>,
    pub looper_context: LooperContext,
    pub tempo_bpm: Option<i32>,
    /// Compiler capture locks `+0x3409`/`+0x3411` (`0x582db8`, `0x582d08`): set from the NC
    /// Recorder start until "Reverting to normal operation" (`0x528498`). A deploy then sends
    /// the gate enables and `0x3b` with 0.
    pub capture_lock: bool,
    /// `/opt/neuraldsp/nopoll` is a file ([`Marker::NoPoll`]): a Looper X replaced by a
    /// deploy sends no follow-up status request (`0x50f360`).
    pub nopoll: bool,
    /// The live preset the deployment shows: driver scene hooks read its scene-mode values
    /// (`0x2ea908`, `0x2ea164`). `None`: no hook sees a scene-mode value.
    pub preset: Option<&'a tetrad_preset::Preset>,
}

impl<'a> Context<'a> {
    pub fn new(specs: &'a Specs, deployment: &'a Deployment, settings: &Settings) -> Self {
        Self {
            specs,
            deployment,
            global_bypass: settings.global_bypass,
            tuner_active: false,
            tuner_input: u32::try_from(settings.tuner.input.to_stock()).unwrap_or_default(),
            tuner_muted: false,
            media: None,
            expression: None,
            looper_context: LooperContext::default(),
            tempo_bpm: None,
            capture_lock: false,
            nopoll: false,
            preset: None,
        }
    }

    fn push(&self, licences: Option<Licences>) -> Push<'a> {
        Push {
            specs: self.specs,
            plan: self.deployment.plan(),
            layout: self.deployment.dsp_layout(),
            scene: self.deployment.scene(),
            global_bypass: self.global_bypass,
            tuner: Tuner {
                engine: self.tuner_active,
                port: self.tuner_input,
                muted: self.tuner_muted,
            },
            media: self.media,
            expression: self.expression,
            looper_context: self.looper_context,
            capture_lock: self.capture_lock,
            nopoll: self.nopoll,
            preset: self.preset,
            licences,
        }
    }
}

/// The app media library as one push sees it, keyed by the block string values (`ir selector`
/// and `IR PATH` values, capture `file_name` keys). The device resolves it before the push,
/// so files and keys stay with the platform; a value that is absent loads the stock default
/// IR (`0x51284c`) or no capture.
#[derive(Debug, Default)]
pub struct ResolvedMedia {
    pub cabs: HashMap<String, CabIrs>,
    pub irs: HashMap<String, IrSamples>,
    pub captures: HashMap<String, CaptureData>,
}

/// The resolved inputs of one push.
#[derive(Clone, Copy)]
struct Push<'a> {
    specs: &'a Specs,
    plan: &'a DeploymentPlan,
    layout: &'a Layout,
    scene: SceneIndex,
    global_bypass: GlobalBypassRows,
    tuner: Tuner,
    media: Option<&'a ResolvedMedia>,
    expression: Option<&'a super::features::Expression>,
    looper_context: LooperContext,
    capture_lock: bool,
    nopoll: bool,
    preset: Option<&'a tetrad_preset::Preset>,
    licences: Option<Licences>,
}

/// One grid block after setup.
#[derive(Debug)]
enum Setup {
    Ready(Block),
    /// Setup was refused; the block holds a muted bypass.
    Muted,
    Looper,
}

/// The booted DSP: session, model windows, the blob key and the deployed grid blocks.
pub struct Dsp<P: Platform> {
    session: DspSession<P::Window, P::Clock>,
    cores: CoreWindows<P::Window>,
    /// `0x25`; kept in memory only.
    blob_key: Option<BlobKey>,
    /// The model blobs read so far ([`Self::core_blob`]).
    blobs: KeptBlobs,
    /// Every driver table from the device's own `/opt/neuraldsp/ZenUI`.
    tables: Option<DriverTables>,
    looper_tables: Option<LooperTables>,
    /// The stock lane object (controller `+0xb910`), kept between pushes.
    lane_io: LaneIo,
    /// Sidechain route records (DSP controller `+0xee64`), kept between pushes.
    sidechains: Sidechains,
    /// Grid blocks of the deployed plan, for edits and scene recall.
    blocks: BTreeMap<Placement, Setup>,
    loopers: BTreeMap<Slot, Looper>,
    looper_playback: LooperPlayback,
    /// Looper rig writes (`RigParameter`) for the action reducer; [`Self::take_rig_writes`].
    rig_writes: Vec<(Slot, u32, f32)>,
    /// Driver preset writes for the device model; [`Self::take_linked_edits`].
    linked: Vec<(Slot, LinkedEdit)>,
    /// The product is the Quad Cortex mini (`0x499a60`), for the driver factory.
    mini: bool,
    /// Driver timer deadlines on the session clock ([`Block::take_timer`]).
    timers: BTreeMap<Placement, Duration>,
    /// The last push completed: `blocks` match the run list. False from the run-list clear
    /// of a deploy until its push ends.
    synced: bool,
    /// `0x58276c` ran once since boot (DSP `+0x83e`): later runs enable the cores in
    /// descending order.
    cores_enabled: bool,
    /// The tempo of the last [`Self::set_tempo`] (`0x64c6e0`); a recall replays it.
    bpm: i32,
    /// A Neural Capture preset runs ([`Self::apply_capture`]); user pushes are refused.
    capture_preset: bool,
    /// A mute that outlives deploys ([`Self::hold_mute`]): a deploy ends with it, not
    /// [`UNMUTED`].
    held_mute: Option<u32>,
    /// A DSP fault latched the mute ([`Self::latch_fault_mute`]): every later `0x20` mutes.
    fault_muted: bool,
    /// The last `0x20` value asked for before the fault gate: the value
    /// [`Self::release_fault_mute`] sends.
    ungated: u32,
    /// Owned blobs that replace stock model blobs at deploy ([`super::owned::install`]);
    /// empty by default.
    pub(super) owned: OwnedBlobs,
    /// (O) Generated community blobs by local model ID ([`super::community::install`]).
    pub(super) community: CommunityBlobs,
    /// (O) The stock Volume framework of community blocks, read once ([`Self::volume_template`]).
    template: Option<VolumeTemplate>,
    /// Plugin licences ([`Self::set_licences`]): a Grid block with a ModelRepo `plugin_id` that
    /// is not [`PluginStatus::Active`] deploys bypassed (`0x00521c84`, `0x005280d8`). `None`
    /// (the default): no gate.
    licences: Option<Licences>,
}

/// The model blobs of [`Dsp::core_blob`]. Stock imports every blob and `diffs.blob` once at
/// boot into its map (`0x880880`, controller `+0x1bbc`, from `blobs/core1..4`) and a deploy
/// takes the per-core variant from that map (`0x87b228`): it reads no file. Tetrad reads each at
/// its first deploy (owned: no boot import), so a layout edit, which redeploys the whole
/// layout as stock does (`Chain::dropSlot` `0x59f620` → `0x5122c0`), reads no file either
/// (A3: the QC drop took 247 ms in `DeployLayout`).
#[derive(Default)]
struct KeptBlobs {
    /// Decrypted `blobs/core1/<name>_blob.bin`, by name.
    base: HashMap<String, ModuleBlob>,
    /// Parsed `diffs.blob` of each transport core but core 0.
    diffs: [Option<DiffFile>; 4],
    /// Per-core variants by core and name.
    variants: [HashMap<String, ModuleBlob>; 4],
}

pub struct Booted<P: Platform> {
    pub dsp: Dsp<P>,
    /// Handled per-core load failures; core 1 still loads after a core-2 failure.
    pub soc1_failures: Vec<CoreFailure>,
    /// Core 0 was silent after the first SOC1 restart and SOC1 was restarted once more.
    pub soc1_retried: bool,
    pub soc2: Soc2Status,
    /// `0x24`: decrypts `ModelRepo.xml`. The caller drops it after the catalog load.
    pub repo_key: Option<KeyMaterial>,
    /// `0x27` (controller `+0xf04c`): the NC2 capture record key (`0x515a48` → `0x6acc9c`,
    /// ZC mode 2). The caller hands it to the media library.
    pub capture_key: Option<KeyMaterial>,
}

/// Stock DSP boot (ZenUI `0x8a8420`, `0x89c848`, `0x875394`): SOC1 kernels through
/// loadsharc's booter, the handshake and SOC2 slave boot, then the key pulls and the internal
/// MIDI clock enable of the DSPController ctor tail (`0x877a84`).
pub fn boot<P: Platform>(platform: &mut P, variant: Variant) -> Result<Booted<P>, DspError> {
    let marker = |platform: &mut P, marker| platform.marker(marker).map_err(DspError::platform);
    if marker(platform, Marker::CcesBoot)? {
        return Err(DspError::Unsupported(
            "ccesboot debugger start (corecontrol only)",
        ));
    }
    let firmware = platform.firmware(variant).map_err(DspError::platform)?;
    let booter = StockBooter::parse(&firmware.loadsharc)?;
    let core1 = RawLdr::parse(&firmware.core1, Limits::default())?;
    let core2 = RawLdr::parse(&firmware.core2, Limits::default())?;
    let start = if marker(platform, Marker::SharcsStarted)? {
        Start::Restart
    } else {
        platform
            .set_marker(Marker::SharcsStarted)
            .map_err(DspError::platform)?;
        Start::First
    };
    let mut plan = plan_soc1(&booter, &core1, &core2, variant, start)?;
    let mut bus = platform.boot_bus().map_err(DspError::platform)?;
    let mut soc1_retried = false;
    // Owned (O): a core 0 that stays silent after the handshake gets one more SOC1 restart
    // (about one boot in ten on the QC, 2026-10-15).
    let (soc1, cores, mut session, soc2) = loop {
        let soc1 = execute_soc1(&plan, &mut bus)?;
        let windows = platform.dsp_windows().map_err(DspError::platform)?;
        let mut session = DspSession::new(DspTransport::new(windows.messages)?, platform.clock());
        // Stock starts its uio ring workers here, at SOC1 comms open (`0x8a778c`): from now on
        // no ring waits for the main loop. A retry drops this session, which joins the thread.
        session.start_drain::<P::RingDrain>()?;
        if platform.drill_core0_silence() {
            session.drill_core0_silence();
        }
        let soc2 = if marker(platform, Marker::NoSlaveBoot)? {
            // Stock ignores the handshake result when the slave boot is disabled, but still
            // runs the GPIO 97 probe and the uio2 open (`0x89c848` → `0x8a7bbc`).
            let _ = session.handshake();
            if platform.open_soc2().map_err(DspError::platform)? {
                session.open_soc2();
            }
            Soc2Status::Skipped
        } else {
            let pairs = &firmware.soc2;
            let mut hooks = Soc2Hooks {
                platform: &mut *platform,
                bus: &mut bus,
                attempts: pairs.factory.iter().chain([&pairs.normal]).collect(),
                attempt: None,
            };
            match session.boot(&mut hooks, pairs.factory.is_some()) {
                Ok(SlaveBootOutcome::Soc2Open) => Soc2Status::Open,
                Ok(SlaveBootOutcome::Soc2CommsFailed) => Soc2Status::CommsFailed,
                Ok(SlaveBootOutcome::Soc2Dead) => Soc2Status::Dead,
                Err(SessionError::Platform(error)) if error.is::<Soc2CopyError>() => {
                    Soc2Status::Stalled
                }
                Err(SessionError::ResponseTimeout(what))
                    if what == CORE0_SILENT && !soc1_retried =>
                {
                    soc1_retried = true;
                    plan = plan_soc1(&booter, &core1, &core2, variant, Start::Restart)?;
                    continue;
                }
                Err(error) => return Err(error.into()),
            }
        };
        break (soc1, windows.cores, session, soc2);
    };
    let keys = session.pull_boot_keys()?;
    // The DSPController ctor tail (`0x877a84`, `0x875fc8(this, 1)`): `0x23` EnableInternalMidiClock
    // 1 to core 4 (`CoreId::Control`). Stock ignores the result (a void call), so a failed send
    // does not stop the boot.
    let request = ControlRequest::EnableInternalMidiClock { value: 1 };
    let frame = tetrad_dsp_protocol::controls::RequestFrame::new(request)?;
    let _ = session.send(
        ControlPacket::Controls(&frame),
        super::control_policy(&request),
    );
    Ok(Booted {
        dsp: Dsp {
            session,
            cores,
            blob_key: keys.blob.as_ref().map(BlobKey::from),
            tables: None,
            looper_tables: None,
            lane_io: LaneIo::new(None),
            sidechains: Sidechains::new(),
            blocks: BTreeMap::new(),
            loopers: BTreeMap::new(),
            looper_playback: LooperPlayback {
                cached_state: 1,
                incoming: tetrad_dsp_protocol::responses::LooperStatus::from_words([0; 22]),
                oneshot: false,
                dupwait: false,
                adopt_next_state: false,
                quantize_enabled: false,
            },
            rig_writes: Vec::new(),
            linked: Vec::new(),
            mini: variant == Variant::Atma,
            timers: BTreeMap::new(),
            synced: true,
            cores_enabled: false,
            bpm: DEFAULT_BPM,
            capture_preset: false,
            held_mute: None,
            fault_muted: false,
            ungated: UNMUTED,
            owned: OwnedBlobs::default(),
            community: CommunityBlobs::default(),
            template: None,
            blobs: Default::default(),
            licences: None,
        },
        soc1_failures: soc1.failures,
        soc1_retried,
        soc2,
        repo_key: keys.repo,
        capture_key: keys.third,
    })
}

/// SOC2 copy failure; the boot stalls without marking cores dead.
#[derive(Debug, thiserror::Error)]
#[error("SOC2 image copy failed: {0}")]
pub struct Soc2CopyError(String);

/// Platform steps of the SOC2 slave boot: image pair choice (factory first, then normal),
/// the copy through the boot bus and the post-boot settle and ring open.
struct Soc2Hooks<'a, P: Platform> {
    platform: &'a mut P,
    bus: &'a mut P::BootBus,
    attempts: Vec<&'a Soc2Pair>,
    attempt: Option<usize>,
}

impl<'a, P: Platform> Soc2Hooks<'a, P> {
    fn images(&self) -> Result<Soc2Images<'a>, Soc2CopyError> {
        let pair: &'a Soc2Pair = self.attempts[self.attempt.unwrap_or(0)];
        Soc2Images::new(&pair.spl, &pair.core0).map_err(|error| Soc2CopyError(error.to_string()))
    }
}

impl<P: Platform> tetrad_dsp_host::session::Soc2Boot for Soc2Hooks<'_, P> {
    type Error = Soc2CopyError;

    fn buffer_bytes(&mut self) -> Result<u32, Self::Error> {
        let next = self.attempt.map_or(0, |attempt| attempt + 1);
        self.attempt = Some(next.min(self.attempts.len() - 1));
        Ok(self.images()?.total_len())
    }

    fn write_images(&mut self, physical_address: u32) -> Result<u32, Self::Error> {
        self.write_image_pair(physical_address)
            .map(|(first, _)| first)
    }

    /// The copy plan comes from tetrad-loader's `Soc2Boot` (`0x89bae0`); the platform maps the
    /// DSP-given buffer first. Both lengths go to `0x13` (first at `+12`, second at `+4`).
    fn write_image_pair(
        &mut self,
        physical_address: u32,
    ) -> Result<(u32, Option<Soc2ImageLengths>), Self::Error> {
        let copy_error = |error: &dyn std::fmt::Display| Soc2CopyError(error.to_string());
        let images = self.images()?;
        let lengths = Soc2ImageLengths {
            first: images.spl_len(),
            second: images.core0_len(),
        };
        let mut copy = Soc2Boot::new(images, None);
        copy.begin().map_err(|error| copy_error(&error))?;
        let (plan, _) = copy
            .on_buffer_address(PhysAddr(physical_address))
            .map_err(|error| copy_error(&error))?;
        self.platform
            .map_soc2_buffer(self.bus, PhysAddr(physical_address), images.total_len())
            .map_err(|error| copy_error(&error))?;
        execute(&plan, self.bus).map_err(|error| copy_error(&error))?;
        Ok((lengths.first, Some(lengths)))
    }

    fn finish(&mut self) -> Result<bool, Self::Error> {
        self.platform.clock().sleep(SETTLE);
        self.platform
            .open_soc2()
            .map_err(|error| Soc2CopyError(error.to_string()))
    }
}

impl<P: Platform> Dsp<P> {
    /// Pushes from the user deployment need the user preset on the DSP.
    fn user_preset(&self) -> Result<(), DspError> {
        if self.capture_preset {
            Err(DspError::CapturePreset)
        } else {
            Ok(())
        }
    }

    pub fn session(&self) -> &DspSession<P::Window, P::Clock> {
        &self.session
    }

    /// Every driver table from the device's own `/opt/neuraldsp/ZenUI`, pinned by SHA-256
    /// and checked once (`DriverTables::load`). Without them, table drivers are muted at
    /// deploy.
    pub fn load_tables(
        &mut self,
        zenui: tetrad_dsp_host::setup::HashedZenUi,
    ) -> Result<(), DspError> {
        let tables = DriverTables::load(zenui)?;
        self.looper_tables = Some(LooperTables::read(&tables)?);
        self.tables = Some(tables);
        Ok(())
    }

    /// Injected table source; production reads the pinned image through `load_tables`.
    pub fn load_looper_tables(
        &mut self,
        tables: &impl tetrad_dsp_host::setup::StockTables,
    ) -> Result<(), DspError> {
        self.looper_tables = Some(LooperTables::read(tables)?);
        Ok(())
    }

    /// A tempo change (`0x875a48`): every driver's tempo hook, sent at once, then `0x16`.
    /// Blocks start at 120 BPM, so the caller sends the tempo after each preset load too.
    pub fn set_tempo(&mut self, bpm: i32) -> Result<(), DspError> {
        self.bpm = bpm;
        let mut writes = Vec::new();
        self.tempo_writes(&mut writes);
        self.execute_writes(writes)?;
        self.arm_timers();
        Ok(())
    }

    /// `0x519dfc` lane mutes and the sidechain cut, or the capture lane push (`0x5205e8`).
    pub fn apply_tuner(&mut self, context: &Context, lane_push: bool) -> Result<(), DspError> {
        self.user_preset()?;
        if !self.synced {
            return Err(DspError::OutOfSync);
        }
        let push = context.push(self.licences);
        let mut writes = Vec::new();
        if lane_push {
            self.push_lanes(push, &mut writes)?;
        } else {
            let lanes = lanes(push)?;
            self.lane_io
                .mute_tuner_input(&lanes, push.tuner, &mut writes);
            let routing = sidechain_context(push, &lanes, self.lane_io.tuner_cut(), false);
            self.sidechains
                .push(&sidechain_sinks(push)?, &routing, &mut writes);
        }
        self.execute_writes(writes)
    }

    fn build_looper(&self, push: Push, slot: Slot, at: Placement) -> Result<Looper, DspError> {
        let deployed = push
            .plan
            .slot(at)
            .ok_or(DspError::Unsupported("Looper placement"))?;
        let placements = Placements {
            sends: [0, 1]
                .map(|index| {
                    push.plan
                        .placement(Node::Send(Endpoint::Looper { slot, index }))
                        .map(target)
                        .transpose()
                })
                .into_iter()
                .collect::<Result<Vec<_>, _>>()?
                .try_into()
                .expect("two sends"),
            returns: [0, 1]
                .map(|index| {
                    push.plan
                        .placement(Node::Receive(Endpoint::Looper { slot, index }))
                        .map(target)
                        .transpose()
                })
                .into_iter()
                .collect::<Result<Vec<_>, _>>()?
                .try_into()
                .expect("two returns"),
        };
        let tables = self.looper_tables.as_ref().ok_or(TableError::Absent)?;
        // `0x55d89c`: the mini (no P13-16 mode changes, `0x49a754` false) latches its modes.
        let product = if push.looper_context.mode_changes_allowed == Some(false) {
            LooperProduct::Mini
        } else {
            LooperProduct::Qc
        };
        Looper::for_product(
            block_model(push, slot, deployed),
            placements,
            tables,
            product,
        )
        .map_err(|error| DspError::Feature(error.to_string()))
    }

    fn recall_loopers(
        &mut self,
        push: Push,
        mode: ParamSetMode,
        pending: &mut Pending,
    ) -> Result<(), DspError> {
        let mut writes = Vec::new();
        for (&slot, driver) in &mut self.loopers {
            let Some(deployed) = push.plan.block(slot).and_then(|at| push.plan.slot(at)) else {
                continue;
            };
            let mut driver_writes = Vec::new();
            let mut order: Vec<_> = deployed
                .values
                .iter()
                .enumerate()
                .take(deployed.values.len().saturating_sub(2))
                .collect();
            // `0x527248`: a recall of model 27000 sends P12 last when its value is 0.
            if mode == ParamSetMode::Recall
                && block_model(push, slot, deployed) == LOOPER_MODEL
                && order
                    .get(LOOPER_LATE_PARAMETER)
                    .is_some_and(|(_, value)| value.at(push.scene) == 0.0)
            {
                let late = order.remove(LOOPER_LATE_PARAMETER);
                order.push(late);
            }
            for (index, value) in order {
                if mode == ParamSetMode::Recall
                    && push.expression.is_some_and(|expression| {
                        expression
                            .values
                            .contains_key(&(Site::Slot(slot), index as u32))
                    })
                {
                    continue;
                }
                let value = push
                    .expression
                    .and_then(|expression| expression.values.get(&(Site::Slot(slot), index as u32)))
                    .copied()
                    .unwrap_or_else(|| value.at(push.scene));
                if let Err(error) = driver.set_parameter(
                    index as u32,
                    value,
                    mode,
                    Some(&mut self.looper_playback),
                    &push.looper_context,
                    &mut driver_writes,
                ) {
                    pending.note(error.to_string());
                }
            }
            writes.push((slot, driver_writes));
        }
        for (slot, writes) in writes {
            self.execute_looper_writes(slot, writes)?;
        }
        Ok(())
    }

    /// The factory replacing every Looper driver for `push.plan` (`0x50eef8`): an old Looper X
    /// takes the global destructor branch (`0x55d534`, `+0xd4`) when the new grid has no
    /// 27000, then one status request unless `nopoll` (`0x50f360`). The playback stays.
    /// Every driver tears down; the first failed send is returned (stock ignores them).
    fn teardown_loopers(&mut self, push: Push) -> Result<(), DspError> {
        let incoming: Vec<u32> = push
            .plan
            .blocks
            .iter()
            .filter_map(|(&slot, &at)| {
                push.plan
                    .slot(at)
                    .map(|deployed| block_model(push, slot, deployed))
            })
            .collect();
        let mut result = Ok(());
        for (slot, driver) in std::mem::take(&mut self.loopers) {
            let mut writes = Vec::new();
            driver.replace(
                incoming.iter().copied(),
                push.nopoll,
                &mut self.looper_playback,
                &mut writes,
            );
            let sent = self.execute_looper_writes(slot, writes);
            if result.is_ok() {
                result = sent;
            }
        }
        result
    }

    /// Looper writes in order: requests `0x29`–`0x2c` wait 0 / 100 ms, controls use the
    /// stock control policy, and rig writes wait for the reducer ([`Self::take_rig_writes`]).
    fn execute_looper_writes(
        &mut self,
        slot: Slot,
        writes: Vec<LooperWrite>,
    ) -> Result<(), DspError> {
        for write in writes {
            match write {
                LooperWrite::Request(request) => {
                    let frame = tetrad_dsp_protocol::tuner_looper::RequestFrame::new(request);
                    self.send(
                        ControlPacket::TunerLooper(&frame),
                        SendPolicy::new(false, 100),
                    )?;
                }
                LooperWrite::Control(request) => {
                    let frame = tetrad_dsp_protocol::controls::RequestFrame::new(request)?;
                    self.send(
                        ControlPacket::Controls(&frame),
                        super::control_policy(&request),
                    )?;
                }
                LooperWrite::Router(write) => self.execute_writes(vec![write])?,
                LooperWrite::Wait(duration) => self.session.clock_mut().sleep(duration),
                LooperWrite::RigParameter { index, value_bits } => {
                    self.rig_writes
                        .push((slot, index, f32::from_bits(value_bits)));
                }
            }
        }
        Ok(())
    }

    /// The Looper rig writes since the last call (P7 reset on Record, `0x55ed80`). The caller
    /// stores them through the normal parameter path; the driver sends nothing for them.
    pub fn take_rig_writes(&mut self) -> Vec<(Slot, u32, f32)> {
        std::mem::take(&mut self.rig_writes)
    }

    /// The drivers' preset writes since the last call ([`LinkedEdit`]: `0x52382c`,
    /// `0x523644`, `0x526bd4`) after a push, edit, recall, stomp or MIDI message (no driver
    /// timer slot writes the preset). Their DSP writes went out already: the caller stores each value and echoes it, and must not
    /// edit the block again.
    pub fn take_linked_edits(&mut self) -> Vec<(Slot, LinkedEdit)> {
        std::mem::take(&mut self.linked)
    }

    /// Stores the drivers' voice notes since the last call ([`Block::take_voice_notes`],
    /// Minivoicer `0x575010`) by grid slot and returns each voice that changed as (slot,
    /// voice, note): the editor cache `0x61d994` emits `noteChanged` only for a new value.
    /// I: a slot starts silent, and slots not in `plan` are dropped (stock caches by row and
    /// column).
    pub fn store_voice_notes(
        &mut self,
        plan: &DeploymentPlan,
        notes: &mut BTreeMap<Slot, [Option<u8>; 2]>,
    ) -> Vec<(Slot, usize, Option<u8>)> {
        notes.retain(|slot, _| plan.blocks.contains_key(slot));
        let mut changed = Vec::new();
        for (&slot, at) in &plan.blocks {
            if let Some(Setup::Ready(block)) = self.blocks.get_mut(at)
                && let Some(sent) = block.take_voice_notes()
            {
                let old = notes.insert(slot, sent).unwrap_or_default();
                let voices = old.into_iter().zip(sent).enumerate();
                changed.extend(
                    voices.filter_map(|(voice, (old, new))| {
                        (old != new).then_some((slot, voice, new))
                    }),
                );
            }
        }
        changed
    }

    fn collect_linked_edits(&mut self, plan: &DeploymentPlan) {
        for (&slot, at) in &plan.blocks {
            if let Some(Setup::Ready(block)) = self.blocks.get_mut(at) {
                let edits = block.take_linked_edits();
                self.linked
                    .extend(edits.into_iter().map(|edit| (slot, edit)));
            }
        }
    }

    /// A stomp press or release on the drivers of its assigned `slots` (`+0x40`/`+0x44`
    /// in `0x5239d0`/`0x523f80`); the writes go out at once. Blocks whose driver takes the
    /// edge ([`tetrad_dsp_host::setup::DriverKind::takes_footswitch`]) get no stomp toggle.
    pub fn footswitch(
        &mut self,
        plan: &DeploymentPlan,
        slots: &[Slot],
        pressed: bool,
    ) -> Result<(), DspError> {
        if self.capture_preset || !self.synced {
            return Ok(());
        }
        let mut writes = Vec::new();
        for at in slots.iter().filter_map(|slot| plan.blocks.get(slot)) {
            if let Some(Setup::Ready(block)) = self.blocks.get_mut(at) {
                block.footswitch(pressed, &mut writes);
            }
        }
        self.execute_writes(writes)?;
        self.arm_timers();
        self.collect_linked_edits(plan);
        Ok(())
    }

    /// The controller's MIDI dispatch to grid drivers in grid order (notes `0x514270`, CC
    /// 120/123 `0x5141b8`); `channel` is 1..=16. Writes go out at once.
    pub fn block_midi(
        &mut self,
        plan: &DeploymentPlan,
        channel: u8,
        message: DriverMidi,
    ) -> Result<(), DspError> {
        if self.capture_preset || !self.synced {
            return Ok(());
        }
        let mut writes = Vec::new();
        for at in plan.blocks.values() {
            if let Some(Setup::Ready(block)) = self.blocks.get_mut(at) {
                block.midi(channel, message, &mut writes);
            }
        }
        self.execute_writes(writes)?;
        self.arm_timers();
        self.collect_linked_edits(plan);
        Ok(())
    }

    /// A Looper button edge or MIDI parameter straight to the driver (`+0x30`, source 0):
    /// nothing is stored (`0x55d1bc`).
    pub fn looper_parameter(
        &mut self,
        site: Site,
        index: u32,
        value: f32,
        context: &LooperContext,
    ) -> Result<(), DspError> {
        if !self.synced {
            return Err(DspError::OutOfSync);
        }
        let Site::Slot(slot) = site else {
            return Err(DspError::Unsupported("Looper site"));
        };
        let driver = self
            .loopers
            .get_mut(&slot)
            .ok_or(DspError::Unsupported("a Looper X driver"))?;
        let mut writes = Vec::new();
        driver
            .set_parameter(
                index,
                value,
                ParamSetMode::Live,
                Some(&mut self.looper_playback),
                context,
                &mut writes,
            )
            .map_err(|error| DspError::Feature(error.to_string()))?;
        self.execute_looper_writes(slot, writes)
    }

    /// A status record (`0x12`): the controller dedupe and the driver reconcile
    /// (`0x513c94`, `0x55db24`).
    pub fn looper_status(
        &mut self,
        status: &tetrad_dsp_protocol::responses::LooperStatus,
        context: &LooperContext,
    ) -> Result<(), DspError> {
        let mut writes = Vec::new();
        for (&slot, driver) in &mut self.loopers {
            let mut driver_writes = Vec::new();
            driver
                .status(
                    status,
                    &mut self.looper_playback,
                    context,
                    &mut driver_writes,
                )
                .map_err(|error| DspError::Feature(error.to_string()))?;
            writes.push((slot, driver_writes));
        }
        for (slot, writes) in writes {
            self.execute_looper_writes(slot, writes)?;
        }
        Ok(())
    }

    /// One 50 ms status poll tick (`0x55e05c`); `disabled` is the `nopoll` marker.
    pub fn poll_loopers(&mut self, disabled: bool) -> Result<(), DspError> {
        let polls: Vec<_> = self
            .loopers
            .iter()
            .map(|(&slot, driver)| {
                let mut writes = Vec::new();
                driver.status_tick(disabled, &mut writes);
                (slot, writes)
            })
            .collect();
        for (slot, writes) in polls {
            self.execute_looper_writes(slot, writes)?;
        }
        Ok(())
    }

    pub fn has_loopers(&self) -> bool {
        !self.loopers.is_empty()
    }

    /// The stock driver tables loaded ([`Self::load_tables`]).
    pub fn has_tables(&self) -> bool {
        self.tables.is_some()
    }

    pub fn looper_playback(&self) -> LooperPlayback {
        self.looper_playback
    }

    /// `0x875a48` with the current tempo: every driver's tempo hook (`+0x5c`), then `0x16`.
    fn tempo_writes(&mut self, out: &mut Vec<DspWrite>) {
        let bpm = self.bpm;
        for setup in self.blocks.values_mut() {
            if let Setup::Ready(block) = setup {
                block.tempo_changed(bpm, out);
            }
        }
        out.push(DspWrite::Control(ControlRequest::TempoChange { bpm }));
    }

    /// The earliest driver timer deadline on the session clock.
    pub fn next_timer(&self) -> Option<Duration> {
        self.timers.values().min().copied()
    }

    /// Runs every due driver timer (`Block::timer_elapsed`); its writes go out at once,
    /// outside bulk mode (c-dsp3b §9). A driver may restart its timer.
    pub fn run_timers(&mut self) -> Result<(), DspError> {
        let now = self.session.clock().now();
        let due: Vec<Placement> = self
            .timers
            .iter()
            .filter(|&(_, &deadline)| deadline <= now)
            .map(|(&at, _)| at)
            .collect();
        let mut writes = Vec::new();
        for at in due {
            self.timers.remove(&at);
            if let Some(Setup::Ready(block)) = self.blocks.get_mut(&at) {
                block.timer_elapsed(&mut writes);
            }
        }
        self.execute_writes(writes)?;
        self.arm_timers();
        Ok(())
    }

    /// (Re)starts the timers the drivers asked for since the last call.
    fn arm_timers(&mut self) {
        let now = self.session.clock().now();
        for (&at, setup) in &mut self.blocks {
            if let Setup::Ready(block) = setup
                && let Some(after) = block.take_timer()
            {
                self.timers.insert(at, now + after);
            }
        }
    }

    /// Next drained response, in stock ring order. Polls the rings when the queue is empty.
    pub fn next_event(&mut self) -> Result<Option<(CoreId, DspEvent)>, DspError> {
        if let Some(event) = self.session.next_event() {
            return Ok(Some(event));
        }
        self.session.poll()?;
        Ok(self.session.next_event())
    }

    /// One recovered request with its stock wait and timeout.
    pub fn request(&mut self, request: Request) -> Result<(), DspError> {
        self.session.request(request)?;
        Ok(())
    }

    /// The plugin licences later pushes gate with; set at boot and after each `0x20` answer.
    pub fn set_licences(&mut self, licences: Licences) {
        self.licences = Some(licences);
    }

    /// `0x20` to cores 0 and 1 (`0x876470`). `value = mute | flag << 1`. After a fault every
    /// value goes out as [`DEPLOY_MUTE`].
    pub fn set_master_mute(&mut self, value: u32) -> Result<(), DspError> {
        let value = self.gated(value);
        Ok(self.session.set_master_mute(value)?)
    }

    /// Owned audio safety for a DSP fault ([`DspFault`]): mute now and hold every later `0x20`
    /// (deploy ends, standby, capture, power) muted while the DSPErrorDialog shows. Stock does
    /// not mute there (O): its crash slot `0x5149dc` only shows the dialog and sets the latch.
    /// REBOOT mutes in the power path (`0x6a8914`); CANCEL releases the hold
    /// ([`Self::release_fault_mute`]), as stock plays on after CANCEL (`0x514c48`).
    pub fn latch_fault_mute(&mut self) -> Result<(), DspError> {
        self.fault_muted = true;
        Ok(self.session.set_master_mute(DEPLOY_MUTE)?)
    }

    /// CANCEL on the DSPErrorDialog (`0x514c48`): the fault mute ends and the `0x20` the device
    /// asked for last goes out, so the unit plays on as stock does. A later fault mutes again.
    pub fn release_fault_mute(&mut self) -> Result<(), DspError> {
        self.fault_muted = false;
        Ok(self.session.set_master_mute(self.ungated)?)
    }

    /// The fault mute holds ([`Self::latch_fault_mute`]).
    pub fn fault_muted(&self) -> bool {
        self.fault_muted
    }

    /// A fault the session queued and the drain (`Device::on_dsp`) has not seen yet (a crash
    /// or non-zero error flags, often raised inside a deploy) latches the mute first, so no
    /// `0x20` unmutes after it (review r23 K3).
    fn gated(&mut self, value: u32) -> u32 {
        self.ungated = value;
        self.fault_muted |= self.session.queued_events().any(|(_, event)| {
            matches!(
                event,
                DspEvent::Crash(_) | DspEvent::ErrorFlagsChanged { flags: 1.., .. }
            )
        });
        if self.fault_muted { DEPLOY_MUTE } else { value }
    }

    /// The update install mute (`0x4e63a4` → `0x876470(1, 1)`) holds while the installer runs:
    /// a deploy in that time ends with `value` instead of [`UNMUTED`] (review r14 S-1).
    pub fn hold_mute(&mut self, value: Option<u32>) {
        self.held_mute = value;
    }

    /// Global audio settings after boot: dynamic delay compensation (`0x4c`, `0x515934`) and
    /// the master volume assignment (`0x3a` × 4, `0x001fc488` → `0x511c7c`; outputs 0..=3
    /// are OUT 1/2, OUT 3/4, SEND 1/2 and headphones, the `0x001fbd38` key order). The
    /// output calibration gains (`0x56`) go out with the UI build ([`output_calibration`]).
    pub fn apply_globals(&mut self, settings: &Settings) -> Result<Pending, DspError> {
        self.session
            .set_dynamic_delay_compensation(settings.general.dynamic_delay_compensation)?;
        let volume = settings.master_volume;
        for (output, assigned) in
            (0..).zip([volume.out12, volume.out34, volume.send12, volume.headphones])
        {
            self.session.request(Request::SetMasterVolumeAssignment {
                output,
                value_bits: f32::from(u8::from(assigned)).to_bits(),
            })?;
        }
        Ok(Pending::default())
    }

    /// The four digital trims of the stock gain scheduler (`0x7d65c4`): inputs 1, 2 and
    /// returns 1, 2 as `0x05` selectors 1, 2, 4, 5. `None` sends 0 to all four, as stock does
    /// while a PGA step changes or one of its mutes (`io+0xc`, `+0xd`) holds; the Neural
    /// Capture override is the external mute `+0xc` (`0x7d6440`, `jacks::InputChange`).
    /// Each trim is `tetrad_hal::zenjack::GainPlan::trim`.
    pub fn set_input_trims(&mut self, trims: Option<[f32; 4]>) -> Result<(), DspError> {
        for (input, trim) in [1, 2, 4, 5].into_iter().zip(trims.unwrap_or([0.0; 4])) {
            self.session.request(Request::SetInputGain {
                input,
                gain_bits: trim.to_bits(),
            })?;
        }
        Ok(())
    }

    /// [`Self::apply`] for the DSPController capture swap (`0x528498`): a capture preset
    /// (`capture: true`) or the user preset again. A capture preset counts once its push
    /// succeeds; the user reload clears the flag first, so a failed reload leaves the DSP
    /// out of sync ([`DspError::OutOfSync`]) and the next preset load redeploys.
    pub fn apply_capture(
        &mut self,
        platform: &mut P,
        update: &Update,
        context: &Context,
        capture: bool,
    ) -> Result<Pending, DspError> {
        if !capture {
            self.capture_preset = false;
        }
        let pending = self.apply_any(platform, update, context)?;
        self.capture_preset = capture;
        Ok(pending)
    }

    /// Applies one tetrad-engine update in stock form (`0x585464`):
    /// - `Redeploy`: master mute, `0x19` LimiterBypass, 20 ms, run-list clear and shared blob
    ///   loads (`DspSession::deploy_loads`), the driver factory, gate and output EQ enables,
    ///   the push in bulk `Value16` (the lane step ends with the sidechain step), alignment,
    ///   `0x1f` (1) to cores 0..=3, unmute;
    /// - `Reuse`: master mute, 20 ms, `0x1f` (0) to cores 0..=3, factory, gate and output EQ
    ///   enables, push in bulk `Value8` without the router setup, alignment, `0x1f` (1),
    ///   unmute. After a failed deploy a reuse redeploys: the run list is not the plan's;
    /// - `Ops`: live edits through the blocks (`0x51e4d8`): `0x02`/`0x03`, bypass `0x0c`,
    ///   sidechain flags and re-routes ([`Sidechains::route`]), alignment, strings through
    ///   the text hook, sent at once.
    ///
    /// A `WontFit` sends nothing: the old run list keeps its drivers. Any other deploy
    /// failure leaves the DSP out of sync ([`DspError::OutOfSync`]) until a deploy succeeds.
    /// Scene changes use [`Self::recall_scene`] for the stock recall order. Refused while a
    /// capture preset runs ([`DspError::CapturePreset`]).
    pub fn apply(
        &mut self,
        platform: &mut P,
        update: &Update,
        context: &Context,
    ) -> Result<Pending, DspError> {
        self.user_preset()?;
        self.apply_any(platform, update, context)
    }

    fn apply_any(
        &mut self,
        platform: &mut P,
        update: &Update,
        context: &Context,
    ) -> Result<Pending, DspError> {
        let push = context.push(self.licences);
        match update {
            Update::Redeploy { plan, ops, .. } => {
                self.redeploy(platform, Push { plan, ..push }, ops)
            }
            Update::Reuse(ops) if !self.synced => self.redeploy(platform, push, ops),
            Update::Reuse(ops) => self.reuse(push, ops),
            Update::Ops(ops) => self.edit(push, ops),
        }
    }

    fn redeploy(
        &mut self,
        platform: &mut P,
        push: Push,
        ops: &[PlanOp],
    ) -> Result<Pending, DspError> {
        let loads = self.core_loads(platform, push)?;
        // Preflight before the mute and the Looper teardown: WontFit keeps playback and
        // polling.
        for load in &loads {
            let blobs: Vec<_> = load.blobs.iter().map(|load| load.blob.as_ref()).collect();
            blob::place(&blobs).map_err(|error| match error {
                blob::BlobError::WontFit(section) => DspError::WontFit {
                    core: load.core,
                    section,
                },
                error => DspError::from(error),
            })?;
        }
        self.session.set_master_mute(DEPLOY_MUTE)?;
        let mut pending = Pending::default();
        // Both deploy paths tear the Looper down after the mute; here it must also come
        // before the run-list clear, so `setup` finds no driver left.
        let result = self.teardown_loopers(push).and_then(|()| {
            // `0x511ee0`, redeploys only; stock ignores its result.
            if let Err(error) = self
                .session
                .request(Request::LimiterBypass { value: DEPLOY_FLAG })
            {
                pending.note(format!("0x19 LimiterBypass not sent: {error}"));
            }
            self.session.clock_mut().sleep(DEPLOY_SETTLE);
            match self.session.deploy_loads(&mut self.cores, &loads) {
                // Nothing was sent: the old run list keeps its blocks.
                Err(SessionError::WontFit { core, section }) => {
                    Err(DspError::WontFit { core, section })
                }
                loaded => {
                    self.synced = false;
                    self.blocks.clear();
                    self.timers.clear();
                    loaded
                        .map_err(DspError::from)
                        .and_then(|()| self.setup(push, PushKind::Redeploy, ops, &mut pending))
                }
            }
        });
        let end = self.gated(self.held_mute.unwrap_or(UNMUTED));
        let unmute = self.session.set_master_mute(end);
        result?;
        if let Err(error) = unmute {
            // The caller keeps its previous plan on an error; the run list is the new one.
            self.synced = false;
            return Err(error.into());
        }
        Ok(pending)
    }

    /// Same layout (`0x5848b0` result 1): no model is reloaded. Stock logs a failed `0x1f`
    /// (0) and carries on, so it is a [`Pending`] note. Once the push is complete the DSP
    /// runs the new values: a failed unmute is a note too, and the caller commits.
    fn reuse(&mut self, push: Push, ops: &[PlanOp]) -> Result<Pending, DspError> {
        self.session.set_master_mute(DEPLOY_MUTE)?;
        self.session.clock_mut().sleep(DEPLOY_SETTLE);
        let mut pending = Pending::default();
        self.enable_cores(0, &mut pending);
        let result = self.setup(push, PushKind::Reuse, ops, &mut pending);
        let end = self.gated(self.held_mute.unwrap_or(UNMUTED));
        let unmute = self.session.set_master_mute(end);
        result?;
        if let Err(error) = unmute {
            pending.note(format!("0x20 unmute after the push failed: {error}"));
        }
        Ok(pending)
    }

    /// `0x1f` EnableAll to cores 0..=3: `0x58276c` sends 1 at the end of every deploy,
    /// ascending the first time after boot, then descending; the reuse sends 0 first,
    /// ascending. Stock logs failures and carries on: each is a [`Pending`] note, except a
    /// dead core, which stock `sendMessage` refuses quietly.
    fn enable_cores(&mut self, value: u32, pending: &mut Pending) {
        let mut cores = DSP_CORES;
        if value == 1 {
            if self.cores_enabled {
                cores.reverse();
            }
            self.cores_enabled = true;
        }
        for core in cores {
            match self.session.request(Request::EnableAll { core, value }) {
                Ok(_) | Err(SessionError::DeadCore(_)) => {}
                Err(error) => pending.note(format!("0x1f ({value}) to {core:?} not sent: {error}")),
            }
        }
    }

    /// Plan cores → session loads: one load per entry of the core's blob list, decrypted and
    /// patched for the core, so slots that share a blob load it once (c-dsp5 §1). Start record
    /// words: `+6` the plan's layout word (`0x87a314`), `+8` the ModelRepo `plugin_id` of a
    /// grid block or −1 (`0x879100`, c-dsp2 §4). On DSP core 0 an owned model's load is its
    /// plaintext owned blob ([`OwnedBlobs`]); every owned decision goes to the log.
    /// The variant of blob `name` for `core` (`0x87b228`). Each blob file, each `diffs.blob`
    /// and each variant is read, decrypted or patched once per run ([`KeptBlobs`]).
    fn core_blob(
        &mut self,
        platform: &mut P,
        core: DspCore,
        name: &str,
    ) -> Result<ModuleBlob, DspError> {
        let kept = &mut self.blobs;
        if let Some(variant) = kept.variants[core as usize].get(name) {
            return Ok(variant.clone());
        }
        if !kept.base.contains_key(name) {
            let key = self.blob_key.as_ref().ok_or(DspError::MissingKey("blob"))?;
            let encrypted = (platform.dsp_file(DspFile::Blob(name))).map_err(DspError::platform)?;
            kept.base
                .insert(name.to_owned(), blob::decrypt(key, &encrypted)?);
        }
        let diffs = &mut kept.diffs[core as usize];
        if core != DspCore::Core0 && diffs.is_none() {
            let file = (platform.dsp_file(DspFile::Diffs(core))).map_err(DspError::platform)?;
            *diffs = Some(DiffFile::parse(&file)?);
        }
        let diff = diffs.as_ref().and_then(|file| file.get(name));
        let variant = blob::for_core(name, &kept.base[name], core, diff)?;
        kept.variants[core as usize].insert(name.to_owned(), variant.clone());
        Ok(variant)
    }

    /// (O) The stock Volume blob of this unit and its records for transport cores 1..=3: the
    /// framework of every community block (`qcdsp::device::VolumeTemplate`), with the kernel
    /// heap call addresses of each core, read from a stock blob that calls the heap
    /// (`qcdsp::device::template::KERNEL_CALL_SOURCE`). qcdsp refuses a blob that is not the
    /// Volume blob of a supported CorOS build.
    pub(super) fn volume_template(
        &mut self,
        platform: &mut P,
    ) -> Result<&VolumeTemplate, DspError> {
        const VOLUME: &str = "volume";
        if self.template.is_none() {
            let community =
                |error: qcdsp::Error| DspError::Feature(format!("community blocks: {error}"));
            let (volume, volume_diffs) = self.blob_and_records(platform, VOLUME)?;
            let calls = qcdsp::device::template::KERNEL_CALL_SOURCE;
            let (calls, call_diffs) = self.blob_and_records(platform, calls)?;
            let mut kernel = qcdsp::device::KernelMap::default();
            kernel
                .add_stock_calls(&calls, &call_diffs)
                .map_err(|error| {
                    DspError::Feature(format!("community blocks: kernel map: {error}"))
                })?;
            let template = VolumeTemplate::new(&volume.to_bytes()?, volume_diffs)
                .map_err(community)?
                .with_kernel(kernel);
            self.template = Some(template);
        }
        Ok(self.template.as_ref().expect("set above"))
    }

    /// The decrypted core 1 blob `name` and its records for transport cores 1..=3.
    fn blob_and_records(
        &mut self,
        platform: &mut P,
        name: &str,
    ) -> Result<(ModuleBlob, [qc_format::diff::DiffRecord; 3]), DspError> {
        self.core_blob(platform, DspCore::Core0, name)?;
        let mut diffs = Vec::with_capacity(3);
        for core in [DspCore::Core1, DspCore::Core2, DspCore::Core3] {
            self.core_blob(platform, core, name)?;
            let file = self.blobs.diffs[core as usize].as_ref();
            let diff = file.and_then(|file| file.get(name)).cloned();
            diffs.push(diff.ok_or(tetrad_dsp_host::blob::BlobError::MissingDiff(core))?);
        }
        let diffs = diffs.try_into().expect("three transport cores");
        Ok((self.blobs.base[name].clone(), diffs))
    }

    fn core_loads(&mut self, platform: &mut P, push: Push) -> Result<Vec<CoreLoad>, DspError> {
        let mut cores = Vec::new();
        for core_plan in &push.plan.cores {
            if core_plan.slots.is_empty() {
                continue;
            }
            let core = DspCore::try_from(core_plan.core.index() as u8)?;
            let mut blobs = Vec::with_capacity(core_plan.blobs.len());
            for load in &core_plan.blobs {
                if let Some(community) = self.community.load(core, load.model.0) {
                    blobs.push(community?);
                    continue;
                }
                if let Some(owned) = self.owned.load(core, load.model.0) {
                    let detail = match &owned {
                        OwnedLoad::Owned(model, _) => format!("owned {model} loads on {core:?}"),
                        OwnedLoad::Stock(model) => format!(
                            "owned {model} is not loaded on {core:?}: only {OWNED_CORE:?} runs \
                             owned blocks; the stock blob loads"
                        ),
                    };
                    platform.log(&super::Notice {
                        at: self.session.clock().now(),
                        subsystem: super::Subsystem::Dsp,
                        detail,
                    });
                    if let OwnedLoad::Owned(_, owned) = owned {
                        blobs.push(owned);
                        continue;
                    }
                }
                let blob = match load.name.as_deref() {
                    None => None,
                    Some(name) => Some(self.core_blob(platform, core, name)?),
                };
                blobs.push(BlobLoad {
                    model_id: load.model.0,
                    blob,
                });
            }
            let mut slots = vec![None; core_plan.slots.len()];
            for deployed in &core_plan.slots {
                let block_word = match deployed.node {
                    Node::Model(Site::Slot(slot)) => push
                        .specs
                        .models
                        .get(&block_model(push, slot, deployed))
                        .and_then(|spec| spec.plugin_id)
                        .and_then(|id| i32::try_from(id).ok())
                        .unwrap_or(-1),
                    _ => -1,
                };
                // Routers (22000..=22004) name no blob.
                slots[deployed.placement.slot.index()] = Some(RunSlot {
                    model_id: deployed.model.0,
                    blob: deployed.blob,
                    layout_word: deployed.layout,
                    block_word,
                });
            }
            cores.push(CoreLoad { core, blobs, slots });
        }
        Ok(cores)
    }

    /// The factory (`0x50eef8`), then the gate enable (`0x5184fc`) and the output EQ enable
    /// (`0x515034`), then stock push all (`0x583078`) in the engine's push order: compiler
    /// router setup (`0x582e80`, run-list loads only), junction heads and router writes
    /// (`0x51e2b8`, `0x511548`), every grid block (`0x5174a8`) when its first op comes, then
    /// the lane I/O step last (`0x5205e8`, tetrad-dsp-host `LaneIo`, which replaces the engine's
    /// lane ops) with its sidechain step (`0x585f04`, which replaces the engine's `Sidechain`
    /// ops); after the bulk flush the alignment (`0x87c11c`) and the core enable (`0x58276c`).
    fn setup(
        &mut self,
        push: Push,
        kind: PushKind,
        ops: &[PlanOp],
        pending: &mut Pending,
    ) -> Result<(), DspError> {
        self.teardown_loopers(push)?;
        self.synced = false;
        self.blocks.clear();
        self.timers.clear();
        let plan = push.plan;
        let (mut constructors, mut writes) = (Vec::new(), Vec::new());
        // Only the enables (`0x5184fc`, `0x515034`): the lane I/O step sends the per-row gate
        // messages in stock lane order. The capture lock turns both off.
        for message in plan.gate_enables(push.capture_lock) {
            writes.push(DspWrite::Control(gate_request(message)));
        }
        writes.push(DspWrite::Control(ControlRequest::OutputEqualizerEnable {
            value: u32::from(plan.output_eq_enabled(push.capture_lock)),
        }));
        if kind == PushKind::Redeploy {
            push_routers(&router_setups(plan)?, &mut writes);
        }
        let handles: BTreeMap<Placement, Slot> =
            plan.blocks.iter().map(|(&slot, &at)| (at, slot)).collect();
        let mut later = Vec::new();
        for op in ops {
            match *op {
                PlanOp::SetParam { at, .. }
                | PlanOp::SetBypass { at, .. }
                | PlanOp::SidechainActive { sink: at, .. }
                    if handles.contains_key(&at) =>
                {
                    if self.blocks.contains_key(&at) {
                        continue;
                    }
                    let slot = handles[&at];
                    if plan.slot(at).is_some_and(|deployed| {
                        matches!(block_model(push, slot, deployed), 27000 | 27002)
                    }) {
                        match self.build_looper(push, slot, at) {
                            Ok(driver) => {
                                self.loopers.insert(slot, driver);
                                self.blocks.insert(at, Setup::Looper);
                                self.looper_playback.adopt_next_state = true;
                            }
                            Err(error) => {
                                pending.note(format!("Looper {slot:?}: {error}"));
                                self.blocks.insert(at, Setup::Muted);
                            }
                        }
                        continue;
                    }
                    let built = self.block(push, slot, at, &mut constructors, &mut writes, pending);
                    let setup = match built {
                        Ok(block) => Setup::Ready(block),
                        Err(reason) => {
                            writes.push(DspWrite::Bypass {
                                target: target(at)?,
                                state: BypassState::BypassedFlagged,
                            });
                            let model = plan
                                .slot(at)
                                .map_or(0, |deployed| block_model(push, slot, deployed));
                            pending.note(format!("model {model} muted: {reason}"));
                            Setup::Muted
                        }
                    };
                    self.blocks.insert(at, setup);
                }
                PlanOp::PushJunctions { pair } => push_junctions(push, pair, &mut writes)?,
                PlanOp::SetParam { at, index, value } => {
                    writes.push(parameter(target(at)?, index, value));
                }
                PlanOp::SetBypass { at, bypassed } => writes.push(DspWrite::Bypass {
                    target: target(at)?,
                    state: engaged_or_bypassed(bypassed),
                }),
                PlanOp::SidechainActive {
                    router: Some(router),
                    active,
                    ..
                } => writes.push(sidechain_active(router, active)?),
                PlanOp::SidechainActive { router: None, .. }
                | PlanOp::Sidechain { .. }
                | PlanOp::Text { .. }
                | PlanOp::Junction { .. }
                | PlanOp::Lane { .. } => {}
                PlanOp::PairDelay { .. } => later.push(*op),
            }
        }
        self.push_lanes(push, &mut writes)?;
        self.session.run_setup(
            &mut self.cores,
            &self.tables.as_ref(),
            BulkMode::from(kind),
            &constructors,
            &writes,
        )?;
        self.arm_timers();
        self.collect_linked_edits(plan);
        // Reuse `0x87d0d8`: zero the ten alignment delays before the alignment.
        let mut writes = if kind == PushKind::Reuse {
            plan.delay_resets()
                .iter()
                .map(router_write)
                .collect::<Result<Vec<_>, _>>()?
        } else {
            Vec::new()
        };
        if !later.is_empty() {
            writes.extend(delay_writes(plan, push.scene)?);
        }
        self.execute_writes(writes)?;
        self.recall_loopers(push, ParamSetMode::Push, pending)?;
        self.enable_cores(1, pending);
        self.synced = true;
        Ok(())
    }

    /// Factory step and block push for one grid block. Constructor and push writes are kept
    /// only when both succeed.
    fn block(
        &self,
        push: Push,
        slot: Slot,
        at: Placement,
        constructors: &mut Vec<DspWrite>,
        writes: &mut Vec<DspWrite>,
        pending: &mut Pending,
    ) -> Result<Block, String> {
        let deployed = push
            .plan
            .slot(at)
            .ok_or_else(|| "its placement is not in the plan".to_owned())?;
        let model_id = block_model(push, slot, deployed);
        let spec = push
            .specs
            .models
            .get(&model_id)
            .ok_or_else(|| "no catalog entry".to_owned())?;
        let placed = |node| push.plan.placement(node).map(target).transpose();
        let send = Node::Send(Endpoint::FxSend {
            slot,
            index: u8::from(model_id == FX_LOOP_SECOND_SEND),
        });
        let identity = ModelIdentity {
            target: target(at).map_err(|error| error.to_string())?,
            model_id,
            category: spec.category_id,
            fx_loop_send: placed(send).map_err(|error| error.to_string())?,
        };
        let library = DeviceMedia {
            specs: push.specs,
            resolved: push.media,
        };
        let factory = Factory {
            tables: self.tables.as_ref(),
            library: &library,
            mini: self.mini,
        };
        let (mut constructed, mut pushed) = (Vec::new(), Vec::new());
        let catalog = catalog_parameters(push, slot, spec);
        let mut block = Block::build(identity, &catalog, &factory, &mut constructed)
            .map_err(|error| error.to_string())?;
        note_missing_media(deployed, push, pending);
        for (index, text) in texts(deployed, push.scene) {
            block
                .set_text(index, text, ParamSetMode::Push, &library, &mut pushed)
                .map_err(|error| error.to_string())?;
        }
        let mut values = setup_values(spec, deployed, push.scene);
        if let Some(expression) = push.expression {
            for (index, value) in values.iter_mut().enumerate() {
                if let Some(live) = expression.values.get(&(Site::Slot(slot), index as u32)) {
                    *value = SetupValue::Float(live.to_bits());
                }
            }
        }
        let router =
            placed(Node::Receive(Endpoint::Sidechain(slot))).map_err(|error| error.to_string())?;
        block
            .push(
                &values,
                block_bypass(push, slot, deployed),
                router,
                &mut pushed,
            )
            .map_err(|error| error.to_string())?;
        constructors.append(&mut constructed);
        writes.append(&mut pushed);
        Ok(block)
    }

    /// The lane I/O step (`0x5205e8`, with the tuner tail while the tuner engine runs) for
    /// the plan and scene, then its last call, the sidechain step (`0x585f04`), into the same
    /// bulk window.
    fn push_lanes(&mut self, push: Push, out: &mut Vec<DspWrite>) -> Result<(), DspError> {
        let plan = push.plan;
        self.lane_io.gate_destination = plan.gate.as_ref().map(|gate| gate.core.index() as u8);
        self.lane_io.gate_off = plan.gate.is_none();
        let lanes = lanes(push)?;
        self.lane_io.push(&lanes, push.tuner, out);
        let context = sidechain_context(push, &lanes, self.lane_io.tuner_cut(), true);
        self.sidechains.push(&sidechain_sinks(push)?, &context, out);
        Ok(())
    }

    /// A re-route of one sink for a live edit (`0x51e4d8`, outside bulk mode) or a recalled
    /// value (`0x52570c`, `bulk`). A `sidechain_in` value routes the sink router only (mode
    /// 2). A source value routes the source, pushes the block's bypass (`0x517228`), sends
    /// the alignment (`0x583028`) and routes the sink router again (mode 2).
    fn edit_sidechain(
        &mut self,
        push: Push,
        sink: Placement,
        edited: Option<(Placement, u32)>,
        bulk: bool,
        out: &mut Vec<DspWrite>,
        pending: &mut Pending,
    ) -> Result<(), DspError> {
        let plan = push.plan;
        let at = target(sink)?;
        let Some(route) = sidechain_sinks(push)?
            .into_iter()
            .find(|route| route.placement == at)
        else {
            pending.note(format!("sidechain sink on {sink:?} is not in the plan"));
            return Ok(());
        };
        let lanes = lanes(push)?;
        let context = sidechain_context(push, &lanes, self.lane_io.tuner_cut(), bulk);
        let Some((slot, deployed)) = handle_slot(plan, sink).zip(plan.slot(sink)) else {
            return Ok(());
        };
        let input_param = push
            .specs
            .models
            .get(&block_model(push, slot, deployed))
            .and_then(|spec| spec.sidechain?.input_param);
        if edited.is_some_and(|(at, index)| at == sink && Some(index) == input_param) {
            self.sidechains
                .route(&route, RouteMode::Sink, &context, out);
            return Ok(());
        }
        // Mode 1 (0x51e7ac, 0x51f85c): source records, route requests and the source router.
        self.sidechains
            .route(&route, RouteMode::Source, &context, out);
        match self.blocks.get_mut(&sink) {
            Some(Setup::Ready(block)) => {
                let router = plan
                    .placement(Node::Receive(Endpoint::Sidechain(slot)))
                    .map(target)
                    .transpose()?;
                block.set_bypass(block_bypass(push, slot, deployed), router, out);
            }
            _ => pending.note(format!(
                "bypass of a muted sidechain sink on {sink:?} not sent"
            )),
        }
        out.extend(delay_writes(plan, push.scene)?);
        self.sidechains
            .route(&route, RouteMode::Sink, &context, out);
        Ok(())
    }

    /// A live input gate edit (`0x51e4d8` case 0x6d60, tetrad-engine `set_input_gate`): the
    /// lane input router ops, then the gate messages (`0x31` parameter or `0x32` trim). The
    /// push sends the gate through the lane I/O step instead.
    pub fn apply_gate(
        &mut self,
        context: &Context,
        update: &tetrad_engine::GateUpdate,
    ) -> Result<Pending, DspError> {
        self.user_preset()?;
        let pending = self.edit(context.push(self.licences), &update.ops)?;
        self.send_gate(&update.gate)?;
        Ok(pending)
    }

    /// The gate messages of an INPUT GAIN edit (`0x31` parameter, `0x32` trim), sent at once.
    /// The capture preset edits (`grid_edit`) send them after their ops as well.
    pub fn send_gate(&mut self, gate: &[GateMessage]) -> Result<(), DspError> {
        let writes = gate
            .iter()
            .map(|&message| DspWrite::Control(gate_request(message)));
        self.execute_writes(writes.collect())
    }

    /// Live edits (`0x51e4d8`), sent at once in op order.
    fn edit(&mut self, push: Push, ops: &[PlanOp]) -> Result<Pending, DspError> {
        if !self.synced {
            return Err(DspError::OutOfSync);
        }
        let mut pending = Pending::default();
        let mut writes = Vec::new();
        let mut delays_sent = false;
        // The last edited parameter, and a SOURCE value that stock sends through the driver
        // after the re-route (`0x51e4d8`; the engine re-routes in the `Sidechain` op).
        let mut edited = None;
        let mut selector = Vec::new();
        for op in ops {
            match *op {
                PlanOp::SetParam { at, index, value } => {
                    if let Some(slot) = handle_slot(push.plan, at)
                        && let Some(driver) = self.loopers.get_mut(&slot)
                    {
                        let mut looper_writes = Vec::new();
                        driver
                            .set_parameter(
                                index,
                                value,
                                ParamSetMode::Live,
                                Some(&mut self.looper_playback),
                                &push.looper_context,
                                &mut looper_writes,
                            )
                            .map_err(|error| DspError::Feature(error.to_string()))?;
                        self.execute_writes(std::mem::take(&mut writes))?;
                        self.execute_looper_writes(slot, looper_writes)?;
                        continue;
                    }
                    edited = Some((at, index));
                    let target = target(at)?;
                    match self.blocks.get_mut(&at) {
                        Some(Setup::Ready(block)) => {
                            let source = sidechain_params(push, at)
                                .is_some_and(|[source, _]| source == Some(index));
                            let out = if source { &mut selector } else { &mut writes };
                            // A value the expression engine holds is its move (`0x520074`):
                            // no controller write `0x523644`, so no `+0x60` hook (review r15
                            // R12). Inferred: a touch edit of a held value skips it too.
                            let held = push.expression.is_some_and(|expression| {
                                handle_slot(push.plan, at).is_some_and(|slot| {
                                    expression.values.contains_key(&(Site::Slot(slot), index))
                                })
                            });
                            let edited = if !held {
                                let setup = SetupValue::Float(value.to_bits());
                                block.edit(index, setup, ParamSetMode::Live, out)
                            } else {
                                block.expression_edit(index, value, out)
                            };
                            if let Err(error) = edited {
                                pending.note(error.to_string());
                            }
                        }
                        Some(Setup::Muted) => pending.note(format!(
                            "edit of a muted block on {at:?} not sent (setup refused)"
                        )),
                        Some(Setup::Looper) => {}
                        // Routers and crossover heads have no driver.
                        None => writes.push(parameter(target, index, value)),
                    }
                }
                PlanOp::SetBypass { at, bypassed } => match self.blocks.get_mut(&at) {
                    Some(Setup::Looper) => {}
                    Some(Setup::Ready(block)) => {
                        let bypass = handle_slot(push.plan, at).zip(push.plan.slot(at)).map_or(
                            BlockBypass::default(),
                            |(slot, deployed)| {
                                let mut bypass = block_bypass(push, slot, deployed);
                                if !forced(push, slot, deployed) {
                                    bypass.bypassed = bypassed;
                                }
                                bypass
                            },
                        );
                        // The router flag follows as `SidechainActive`.
                        block.set_bypass(bypass, None, &mut writes);
                    }
                    Some(Setup::Muted) => {
                        pending.note(format!("bypass of a muted block on {at:?} not sent"));
                    }
                    None => writes.push(DspWrite::Bypass {
                        target: target(at)?,
                        state: engaged_or_bypassed(bypassed),
                    }),
                },
                PlanOp::SidechainActive {
                    router: Some(router),
                    active,
                    ..
                } => writes.push(sidechain_active(router, active)?),
                PlanOp::SidechainActive { router: None, .. } => {}
                PlanOp::Sidechain { sink, .. } => {
                    self.edit_sidechain(push, sink, edited, false, &mut writes, &mut pending)?;
                    writes.append(&mut selector);
                }
                PlanOp::PairDelay { .. } if !delays_sent => {
                    delays_sent = true;
                    writes.extend(delay_writes(push.plan, push.scene)?);
                }
                PlanOp::PairDelay { .. } => {}
                PlanOp::Text { at, index } => {
                    self.edit_text(push, at, index, &mut writes, &mut pending);
                }
                PlanOp::PushJunctions { pair } => push_junctions(push, pair, &mut writes)?,
                PlanOp::Junction { site, index } => {
                    let (pair, junction_site, instance) = match site {
                        Site::Splitter(pair) => (pair, JunctionSite::Splitter, site),
                        Site::Mixer(pair) => (pair, JunctionSite::Mixer, site),
                        _ => return Err(DspError::Unsupported("a junction op on a block")),
                    };
                    if let Some(instance) = push.layout.instance(instance) {
                        let mut values = instance.dsp_values(push.scene);
                        apply_expression(push, site, &mut values);
                        junction(push.plan, pair)?.edit(junction_site, index, &values, &mut writes);
                    }
                }
                PlanOp::Lane { row, index } => {
                    let lanes = lanes(push)?;
                    self.lane_io.edit(&lanes, row.index(), index, &mut writes);
                }
            }
        }
        writes.append(&mut selector);
        self.execute_writes(writes)?;
        self.arm_timers();
        self.collect_linked_edits(push.plan);
        Ok(pending)
    }

    /// A live string edit: the block's text hook (`+0x34`, [`ParamSetMode::Live`]) reloads
    /// the IR or capture the plan's scene text names.
    fn edit_text(
        &mut self,
        push: Push,
        at: Placement,
        index: u32,
        out: &mut Vec<DspWrite>,
        pending: &mut Pending,
    ) {
        let Some(deployed) = push.plan.slot(at) else {
            return;
        };
        let text = texts(deployed, push.scene).find(|&(text, _)| text == index);
        let library = DeviceMedia {
            specs: push.specs,
            resolved: push.media,
        };
        match (self.blocks.get_mut(&at), text) {
            (Some(Setup::Ready(block)), Some((_, text))) => {
                note_missing_media(deployed, push, pending);
                let edited = block.set_text(index, text, ParamSetMode::Live, &library, out);
                if let Err(error) = edited {
                    pending.note(error.to_string());
                }
            }
            (Some(Setup::Ready(_)), None) => {
                pending.note(format!(
                    "string {index} on {at:?} has no text in this scene"
                ));
            }
            (Some(Setup::Muted), _) => pending.note(format!(
                "string edit of a muted block on {at:?} not sent (setup refused)"
            )),
            (None, _) => {}
            (Some(Setup::Looper), _) => {}
        }
    }

    pub(crate) fn execute_writes(&mut self, writes: Vec<DspWrite>) -> Result<(), DspError> {
        if !writes.is_empty() {
            self.session
                .execute(&mut self.cores, &self.tables.as_ref(), None, &writes)?;
        }
        Ok(())
    }

    /// Stock scene recall (`0x527248`, order in tetrad-core `recall_plan`) inside bulk `Value8`
    /// (inferred), flushed at each `Flush` step. Each grid block gets its bypass and values
    /// (`0x52570c`, mode Recall), then [`Block::recalled`] (`0x526cc0`); a sidechain sink's
    /// SOURCE and `sidechain_in` values re-route it first, as a live edit does. Row I/O after
    /// each even row is the pair's junction (heads and split/merge routers, from the engine's
    /// push). The tail runs in the last bulk window: the alignment (`0x582fd8`), the lane rows
    /// ([`LaneIo::recall`]: no lane or sidechain step) and the tempo hooks (`0x875a48`).
    /// `metronome` is the resend `0x512c74`, sent outside bulk mode after the bypass pass.
    pub fn recall_scene(
        &mut self,
        context: &Context,
        metronome: Option<&ResendPlan>,
    ) -> Result<Pending, DspError> {
        self.user_preset()?;
        if !self.synced {
            return Err(DspError::OutOfSync);
        }
        let push = context.push(self.licences);
        if let Some(bpm) = context.tempo_bpm {
            self.bpm = bpm;
        }
        let plan = push.plan;
        let mut pending = Pending::default();
        let mut recall = Vec::new();
        // Per grid address: the placement and the sidechain SOURCE and `sidechain_in` indices.
        let mut addresses = BTreeMap::new();
        for (&slot, &at) in &plan.blocks {
            let (Some(Setup::Ready(_)), Some(deployed)) = (self.blocks.get(&at), plan.slot(at))
            else {
                continue;
            };
            let address = BlockAddress {
                row: slot.row.index() as u8,
                column: slot.column.index() as u8,
            };
            let model_id = block_model(push, slot, deployed);
            let spec = push.specs.models.get(&model_id);
            let bypass = block_bypass(push, slot, deployed);
            recall.push(RecallBlock {
                block: address,
                model_id,
                category: spec.map_or(u32::MAX, |spec| spec.category_id),
                bypassed: deployed.bypass.at(push.scene),
                globally_bypassed: forced(push, slot, deployed),
                mute: bypass.muted,
                expression_bypass: push
                    .expression
                    .is_some_and(|expression| expression.bypasses.contains_key(&Site::Slot(slot))),
                // Looper blocks are `Setup::Looper`: `recall_loopers` owns the P12 rule.
                late_parameter_zero: false,
                parameter_count: deployed.values.len().saturating_sub(2),
            });
            addresses.insert(
                (address.row, address.column),
                (slot, at, sidechain_params(push, at)),
            );
        }
        let steps = recall_plan(&recall, |address, index| {
            addresses
                .get(&(address.row, address.column))
                .and_then(|&(_, at, _)| plan.slot(at))
                .is_some_and(|deployed| deployed.skipped.contains(&(index as u32)))
                || addresses
                    .get(&(address.row, address.column))
                    .is_some_and(|&(slot, _, _)| {
                        push.expression.is_some_and(|expression| {
                            expression
                                .values
                                .contains_key(&(Site::Slot(slot), index as u32))
                        })
                    })
        });
        let mut chunks = vec![Vec::new()];
        let mut open: Option<Placement> = None;
        let mut second_pass = false;
        for step in steps {
            if step == RecallStep::Flush {
                second_pass = true;
                chunks.push(Vec::new());
                continue;
            }
            let out = chunks.last_mut().expect("one chunk");
            match step {
                RecallStep::Flush => {}
                RecallStep::RowIo { row } => {
                    self.close(open.take(), push, out);
                    let pair = if row < 2 { Pair::Upper } else { Pair::Lower };
                    push_junctions(push, pair, out)?;
                }
                RecallStep::Bypass { block, value } => {
                    let Some(&(slot, at, _)) = addresses.get(&(block.row, block.column)) else {
                        continue;
                    };
                    if second_pass && open != Some(at) {
                        self.close(open.replace(at), push, out);
                    }
                    let bypass = BlockBypass {
                        bypassed: value != BypassValue::Off,
                        muted: value == BypassValue::Mute,
                    };
                    let router = plan
                        .placement(Node::Receive(Endpoint::Sidechain(slot)))
                        .map(target)
                        .transpose()?;
                    if let Some(Setup::Ready(block)) = self.blocks.get_mut(&at) {
                        block.set_bypass(bypass, router, out);
                    }
                }
                RecallStep::Parameter { block, index } => {
                    let Some(&(_, at, routing)) = addresses.get(&(block.row, block.column)) else {
                        continue;
                    };
                    if open != Some(at) {
                        self.close(open.replace(at), push, out);
                    }
                    let Some(value) = plan
                        .slot(at)
                        .and_then(|deployed| deployed.values.get(index))
                    else {
                        continue;
                    };
                    let index = index as u32;
                    if routing.is_some_and(|routing| routing.contains(&Some(index))) {
                        self.edit_sidechain(push, at, Some((at, index)), true, out, &mut pending)?;
                    }
                    let bits = value.at(push.scene).to_bits();
                    if let Some(Setup::Ready(block)) = self.blocks.get_mut(&at) {
                        let result =
                            block.edit(index, SetupValue::Float(bits), ParamSetMode::Recall, out);
                        if let Err(error) = result {
                            pending.note(error.to_string());
                        }
                    }
                }
            }
        }
        let mut out = chunks.pop().expect("one chunk");
        self.close(open.take(), push, &mut out);
        out.extend(delay_writes(plan, push.scene)?);
        let mut recalled_lanes = lanes(push)?;
        for (index, lane) in recalled_lanes.iter_mut().enumerate() {
            let row = Row::new(index as u8).expect("lane row");
            if push.expression.is_some_and(|expression| {
                expression
                    .values
                    .contains_key(&(Site::OutputControl(row), 0))
            }) {
                lane.scene_volume = lane.volume;
            }
        }
        self.lane_io.recall(&recalled_lanes, &mut out);
        self.tempo_writes(&mut out);
        chunks.push(out);
        for (index, chunk) in chunks.iter().enumerate() {
            if !chunk.is_empty() {
                self.session.execute(
                    &mut self.cores,
                    &self.tables.as_ref(),
                    Some(BulkMode::Value8),
                    chunk,
                )?;
            }
            if index == 0
                && let Some(plan) = metronome
            {
                self.send_metronome(plan)?;
            }
        }
        self.arm_timers();
        self.collect_linked_edits(plan);
        self.recall_loopers(push, ParamSetMode::Recall, &mut pending)?;
        Ok(pending)
    }

    /// The end of one block in a recall: its strings (Recall), then its driver's scene and
    /// after-push hooks (`0x526cc0`).
    fn close(&mut self, at: Option<Placement>, push: Push, out: &mut Vec<DspWrite>) {
        let Some(at) = at else { return };
        let library = DeviceMedia {
            specs: push.specs,
            resolved: push.media,
        };
        let Some(Setup::Ready(block)) = self.blocks.get_mut(&at) else {
            return;
        };
        if let Some(deployed) = push.plan.slot(at) {
            for (index, text) in texts(deployed, push.scene) {
                // A refused string keeps the block's previous IR; the push noted the gap.
                let _ = block.set_text(index, text, ParamSetMode::Recall, &library, out);
            }
        }
        let slot = push.plan.blocks.iter().find(|&(_, &placed)| placed == at);
        let scene_value = |index| {
            let param = super::preset::stored_param(push.preset?, Site::Slot(*slot?.0), index)?;
            match param.value(push.scene.index())? {
                tetrad_preset::ParamValue::Float(value) if param.scene_mode == Some(true) => {
                    Some(*value)
                }
                _ => None,
            }
        };
        block.recalled(&scene_value, out);
    }

    /// One complete packet with an explicit policy, for bridges that own a message family
    /// (Cortex Control `0x27` frames, tuner/looper and metronome controls).
    pub fn send(&mut self, packet: ControlPacket<'_>, policy: SendPolicy) -> Result<(), DspError> {
        self.session.send(packet, policy)?;
        Ok(())
    }

    /// The metronome resend `0x512c74` in plan order, each step with its stock policy.
    pub fn send_metronome(&mut self, plan: &ResendPlan) -> Result<(), DspError> {
        for step in &plan.steps {
            match step {
                ResendStep::Request { frame, policy } => self.send(
                    ControlPacket::Controls(frame),
                    SendPolicy::new(policy.wait, policy.timeout.as_millis() as u32),
                )?,
                ResendStep::Block(block) => self.send(
                    ControlPacket::MetronomeBlock(block),
                    SendPolicy::new(false, 100),
                )?,
                ResendStep::SyncAllCores => {
                    for core in DSP_CORES {
                        let frame = tetrad_dsp_protocol::requests::RequestFrame::new(
                            Request::DiagnosticsRequest {
                                core: CoreId::Dsp(core),
                            },
                        )?;
                        self.send(ControlPacket::Request(&frame), SendPolicy::new(true, 1000))?;
                    }
                }
            }
        }
        Ok(())
    }
}

impl From<PushKind> for BulkMode {
    /// The bulk flag byte is the redeploy flag (c-dsp2 §1): 16 after a load, 8 in place.
    fn from(kind: PushKind) -> Self {
        match kind {
            PushKind::Redeploy => Self::Value16,
            PushKind::Reuse => Self::Value8,
        }
    }
}

pub(super) fn target(at: Placement) -> Result<DeploymentTarget, DspError> {
    Ok(DeploymentTarget {
        core: DspCore::try_from(at.core.index() as u8)?,
        slot: ModuleSlot::try_from(at.slot.index() as u32)?,
    })
}

fn parameter(target: DeploymentTarget, index: u32, value: f32) -> DspWrite {
    DspWrite::Parameter {
        target,
        index,
        value_bits: value.to_bits(),
    }
}

fn engaged_or_bypassed(bypassed: bool) -> BypassState {
    if bypassed {
        BypassState::Bypassed
    } else {
        BypassState::Engaged
    }
}

/// Router parameter 5 = block active (`0x517228`).
fn sidechain_active(router: Placement, active: bool) -> Result<DspWrite, DspError> {
    Ok(DspWrite::IntParameter {
        target: target(router)?,
        index: tetrad_dsp_host::setup::SIDECHAIN_ACTIVE_PARAMETER,
        value: u32::from(active),
    })
}

/// A typed router write: `Int` → `0x03`, `Float` → `0x02` (`0x582e80`).
fn router_write(write: &RouterWrite) -> Result<DspWrite, DspError> {
    let target = target(write.at)?;
    Ok(match write.value {
        RouterValue::Int(value) => DspWrite::IntParameter {
            target,
            index: write.index,
            value: value as u32,
        },
        RouterValue::Float(value) => parameter(target, write.index, value),
    })
}

/// The pair's junction routers (`RouterSet::{split, merge, heads}`).
fn junction(plan: &DeploymentPlan, pair: Pair) -> Result<Junction, DspError> {
    let routers = &plan.routers;
    let placed = |at: Option<Placement>| at.map(target).transpose();
    Ok(Junction {
        split: placed(routers.split[pair as usize])?,
        merge: placed(routers.merge[pair as usize])?,
        heads: [
            placed(routers.heads[pair.master().index()])?,
            placed(routers.heads[pair.slave().index()])?,
        ],
    })
}

/// The pair's splitter then mixer push (`0x51e2b8`, `0x511548`) with their scene values.
fn push_junctions(push: Push, pair: Pair, out: &mut Vec<DspWrite>) -> Result<(), DspError> {
    let junction = junction(push.plan, pair)?;
    let layout = push.layout.pair(pair);
    if let Some(splitter) = layout.splitter.as_ref().filter(|_| layout.split.is_some()) {
        let mut values = splitter.dsp_values(push.scene);
        apply_expression(push, Site::Splitter(pair), &mut values);
        junction.push_splitter(&values, out);
    }
    if let Some(mixer) = layout.mixer.as_ref().filter(|_| layout.merge.is_some()) {
        let mut values = mixer.dsp_values(push.scene);
        apply_expression(push, Site::Mixer(pair), &mut values);
        junction.push_mixer(&values, out);
    }
    Ok(())
}

fn apply_expression(push: Push, site: Site, values: &mut [f32]) {
    if let Some(expression) = push.expression {
        for (index, value) in values.iter_mut().enumerate() {
            if let Some(live) = expression.values.get(&(site, index as u32)) {
                *value = *live;
            }
        }
    }
}

/// The scene's alignment writes (`0x87c11c`, `0x87c958`).
fn delay_writes(plan: &DeploymentPlan, scene: SceneIndex) -> Result<Vec<DspWrite>, DspError> {
    plan.delay_writes[scene.index()]
        .iter()
        .map(router_write)
        .collect()
}

/// Compiler router setup (`0x582e80`) per router.
fn router_setups(plan: &DeploymentPlan) -> Result<Vec<RouterSetup>, DspError> {
    plan.deployed()
        .filter(|deployed| !deployed.setup.is_empty())
        .map(|deployed| {
            let writes = deployed
                .setup
                .iter()
                .map(|write| preblock::RouterWrite {
                    index: write.index,
                    value: match write.value {
                        RouterValue::Int(value) => preblock::RouterValue::Int(value as u32),
                        RouterValue::Float(value) => preblock::RouterValue::Float(value),
                    },
                })
                .collect();
            Ok(RouterSetup {
                target: target(deployed.placement)?,
                writes,
            })
        })
        .collect()
}

/// The grid slot whose values target `at` (`plan.blocks`).
fn handle_slot(plan: &DeploymentPlan, at: Placement) -> Option<Slot> {
    plan.blocks
        .iter()
        .find(|&(_, &placement)| placement == at)
        .map(|(&slot, _)| slot)
}

/// The block's own model id (the factory switches on it, not on a clone's engine).
fn block_model(push: Push, slot: Slot, deployed: &Deployed) -> u32 {
    push.layout
        .instance(Site::Slot(slot))
        .map_or(deployed.model.0, |instance| instance.model.0)
}

/// Runtime values by QVariant type: catalog values are floats, sent unchanged; meters and
/// strings are never pushed (c-dsp2 §3).
fn setup_values(spec: &ModelSpec, deployed: &Deployed, scene: SceneIndex) -> Vec<SetupValue> {
    (0..)
        .zip(&deployed.values)
        .map(
            |(index, value)| match spec.parameter(index).map(|param| &param.kind) {
                Some(kind) if kind.is_meter() => SetupValue::Meter,
                Some(ParameterType::String) => SetupValue::Text,
                _ => SetupValue::Float(value.at(scene).to_bits()),
            },
        )
        .collect()
}

/// The block's parameter info (stock `ParameterInfo`): ModelRepo ranges and `xValues`/
/// `yValues` breakpoints, except that the sidechain SOURCE has this block's dynamic step list (Off, Follow Input, 12 inputs and the
/// preceding blocks, `0x585b2c`; review c-dsp3b4a F8).
fn catalog_parameters(push: Push, slot: Slot, spec: &ModelSpec) -> Vec<CatalogParameter> {
    let source = spec.sidechain.map(|sidechain| sidechain.source_param);
    spec.parameters
        .iter()
        .map(|param| {
            let mut info = CatalogParameter {
                min: param.scale.law.min,
                max: param.scale.law.max,
                skew: param.scale.law.skew,
                steps: param.scale.steps(),
                step: param.scale.step_size(),
                piecewise: param.scale.piecewise.clone(),
            };
            if source == Some(param.index) {
                let steps = tetrad_engine::sidechain_steps(push.layout, slot);
                info.steps = steps as i32;
                info.step = 1.0 / steps.saturating_sub(1).max(1) as f32;
            }
            info
        })
        .collect()
}

/// A sidechain sink's SOURCE and `sidechain_in` parameter indices.
fn sidechain_params(push: Push, at: Placement) -> Option<[Option<u32>; 2]> {
    let plan = push.plan;
    if !plan.sidechains.iter().any(|sink| sink.sink == at) {
        return None;
    }
    let (slot, deployed) = handle_slot(plan, at).zip(plan.slot(at))?;
    let sidechain = push
        .specs
        .models
        .get(&block_model(push, slot, deployed))?
        .sidechain?;
    Some([Some(sidechain.source_param), sidechain.input_param])
}

/// Bypass from the scene and MUTE (the value before BYPASS); a globally bypassed row forces
/// bypass and clears MUTE (`0x41111c`).
fn block_bypass(push: Push, slot: Slot, deployed: &Deployed) -> BlockBypass {
    if forced(push, slot, deployed) {
        return BlockBypass {
            bypassed: true,
            muted: false,
        };
    }
    let count = deployed.values.len();
    BlockBypass {
        bypassed: push
            .expression
            .and_then(|expression| expression.bypasses.get(&Site::Slot(slot)))
            .copied()
            .unwrap_or_else(|| deployed.bypass.at(push.scene)),
        muted: count >= 2 && deployed.values[count - 2].at(push.scene) != 0.0,
    }
}

/// Cab rows force categories 12, 21, 32, 33; IR rows force category 29 (`0x41111c`). A plugin
/// block that is not licensed and active is forced too (`0x00521c84`).
fn forced(push: Push, slot: Slot, deployed: &Deployed) -> bool {
    let spec = push.specs.models.get(&block_model(push, slot, deployed));
    let category = spec.map_or(u32::MAX, |spec| spec.category_id);
    let row = slot.row.index();
    let unlicensed = push
        .licences
        .zip(spec.and_then(|spec| spec.plugin_id))
        .is_some_and(|(licences, plugin)| licences.status(plugin) != PluginStatus::Active);
    (CAB_CATEGORIES.contains(&category) && push.global_bypass.cab[row])
        || (category == IR_LOADER_CATEGORY && push.global_bypass.ir[row])
        || unlicensed
}

/// Notes every string of a cab, IR Loader or capture block that the resolved media lacks:
/// the driver then loads the stock default IR (`0x51284c`) or no capture.
fn note_missing_media(deployed: &Deployed, push: Push, pending: &mut Pending) {
    let resolved = |value: &str| {
        push.media.is_some_and(|media| {
            media.cabs.contains_key(value)
                || media.irs.contains_key(value)
                || media.captures.contains_key(value)
        })
    };
    for (_, value) in texts(deployed, push.scene) {
        if !value.is_empty() && !resolved(value) {
            pending.note(format!(
                "media {value:?} is not in the IR or capture library: the stock default IR \
                 loads, or no capture"
            ));
        }
    }
}

/// The app media library as the cab, IR Loader and capture drivers see it: what the device
/// resolved for this push.
struct DeviceMedia<'a> {
    specs: &'a Specs,
    resolved: Option<&'a ResolvedMedia>,
}

impl MediaLibrary for DeviceMedia<'_> {
    fn plugin_id(&self, model_id: u32) -> Option<u32> {
        self.specs.models.get(&model_id)?.plugin_id
    }

    fn cab_irs(&self, selector: &str) -> CabIrs {
        self.resolved
            .and_then(|media| media.cabs.get(selector))
            .cloned()
            .unwrap_or(CabIrs::Missing)
    }

    fn ir_product(&self, product_id: &str) -> Option<IrSamples> {
        self.resolved?.irs.get(product_id).cloned()
    }

    fn capture(&self, key: &str) -> Option<CaptureData> {
        self.resolved?.captures.get(key).cloned()
    }
}

/// The block's string values in the scene (`ir selector`, `IR PATH`, `file_name`).
fn texts(deployed: &Deployed, scene: SceneIndex) -> impl Iterator<Item = (u32, &str)> {
    deployed.texts.iter().filter_map(move |text| {
        let value = text.scenes.get(scene.index())?.as_deref()?;
        Some((text.index, value))
    })
}

/// The plan's sidechain sinks in the scene for the sidechain step (c-dsp5 §2): the receive
/// router `+0x7b4` (22000, or the sink itself), the source router `+0x6b4` (the SOC2 forward
/// 22002, or the same node), the scene's source, `sidechain_in` level and bypass.
fn sidechain_sinks(push: Push) -> Result<Vec<SidechainSink>, DspError> {
    let plan = push.plan;
    let mut live_layout = None;
    if let Some(expression) = push.expression {
        for sink in &plan.sidechains {
            let Some(instance) = push.layout.instance(Site::Slot(sink.site)) else {
                continue;
            };
            let Some(spec) = push
                .specs
                .models
                .get(&instance.model.0)
                .and_then(|spec| spec.sidechain)
            else {
                continue;
            };
            if let Some(&value) = expression
                .values
                .get(&(Site::Slot(sink.site), spec.source_param))
            {
                let layout = live_layout.get_or_insert_with(|| push.layout.clone());
                if let Some(instance) = layout.instance_mut(Site::Slot(sink.site)) {
                    instance.set_param(spec.source_param, push.scene, value);
                }
            }
        }
    }
    let live_routes = live_layout
        .as_ref()
        .map(|layout| tetrad_engine::route(layout, push.specs))
        .transpose()?;
    let router = |at: Placement| -> Result<LaneRouter, DspError> {
        Ok(LaneRouter {
            target: target(at)?,
            model_id: plan.slot(at).map_or(0, |deployed| deployed.model.0),
        })
    };
    let mut sinks = Vec::with_capacity(plan.sidechains.len());
    for sink in &plan.sidechains {
        let SidechainPlan { sink: at, site, .. } = *sink;
        let Some(deployed) = plan.slot(at) else {
            continue;
        };
        let level = push
            .specs
            .models
            .get(&block_model(push, site, deployed))
            .and_then(|spec| spec.sidechain?.input_param)
            .and_then(|index| {
                push.expression
                    .and_then(|expression| expression.values.get(&(Site::Slot(site), index)))
                    .copied()
                    .or_else(|| {
                        deployed
                            .values
                            .get(index as usize)
                            .map(|value| value.at(push.scene))
                    })
            });
        let source = live_routes
            .as_ref()
            .and_then(|routes| routes.sidechains.iter().find(|route| route.sink == site))
            .map_or(sink.scenes[push.scene.index()].source, |route| {
                route.scenes[push.scene.index()]
            });
        let source = match source {
            SidechainSource::Off => sidechain::SidechainSource::Off,
            SidechainSource::FollowInput => sidechain::SidechainSource::FollowInput,
            SidechainSource::External(port) => sidechain::SidechainSource::Input(port as u32),
            SidechainSource::Block(slot) => sidechain::SidechainSource::Block {
                cell: grid_cell(slot),
                placement: plan.block(slot).map(target).transpose()?,
            },
        };
        let receive = router(sink.router.unwrap_or(at))?;
        sinks.push(SidechainSink {
            cell: grid_cell(site),
            placement: target(at)?,
            source_router: sink.source_router.map_or(Ok(receive), router)?,
            router: receive,
            channel: u32::from(sink.channel),
            slave_channel: sink.slave_channel.unwrap_or(0),
            source,
            level,
            bypassed: block_bypass(push, site, deployed).bypassed,
        });
    }
    Ok(sinks)
}

fn grid_cell(slot: Slot) -> GridCell {
    GridCell::new(slot.row.index() as u8, slot.column.index() as u8)
        .expect("grid slots are in the 4 × 8 grid")
}

/// What the sidechain route reads besides the sink: lanes, pair junctions, the tuner cut
/// ([`LaneIo::tuner_cut`]) and the bulk flag (controller `+0xb8a8`).
fn sidechain_context<'a>(
    push: Push,
    lanes: &'a [Lane; 4],
    tuner_cut: Option<u32>,
    bulk: bool,
) -> SidechainContext<'a> {
    let pair = |pair: Pair| {
        let junction = push.layout.pair(pair);
        PairJunction {
            split: junction.split.map(|column| column.index() as u8),
            merge: junction.merge.map(|column| column.index() as u8),
        }
    };
    SidechainContext {
        lanes,
        pairs: [pair(Pair::Upper), pair(Pair::Lower)],
        tuner_input: tuner_cut,
        bulk,
    }
}

/// The four lanes for the lane I/O step: ports, 23000 VOLUME/PAN/MUTE/SOLO and 28000 values
/// in the scene (catalog defaults for a lane without the control), path B mute
/// (`0x5147cc`: the slave row of a split-only pair while the mixer is bypassed) and the
/// compiler's lane routers. Live expression values replace the stored control values.
fn lanes(push: Push) -> Result<[Lane; 4], DspError> {
    let plan = push.plan;
    let value = |instance: Option<&Instance>, model: u32, index: usize| {
        instance
            .and_then(|instance| instance.params.get(index))
            .map(|value| value.at(push.scene))
            .or_else(|| {
                let spec = push.specs.models.get(&model)?;
                Some(spec.parameter(index as u32)?.default_normalized())
            })
            .unwrap_or(0.0)
    };
    let router = |at: Option<Placement>| -> Result<Option<LaneRouter>, DspError> {
        at.map(|at| {
            Ok(LaneRouter {
                target: target(at)?,
                model_id: plan.slot(at).map_or(0, |deployed| deployed.model.0),
            })
        })
        .transpose()
    };
    let mut lanes = Vec::with_capacity(ROWS);
    for index in 0..ROWS {
        let row = Row::new(index as u8).expect("row in range");
        let lane = push.layout.lane(row);
        let output = lane.output_control.as_ref();
        let input = lane.input_control.as_ref();
        let pair = row.pair();
        let junctions = push.layout.pair(pair);
        let control = |index| {
            push.expression
                .and_then(|expression| {
                    expression
                        .values
                        .get(&(Site::OutputControl(row), index as u32))
                })
                .copied()
                .unwrap_or_else(|| value(output, ids::LANE_OUTPUT_CONTROL, index))
        };
        lanes.push(Lane {
            input: lane.input as u32,
            output: lane.output as u32,
            volume: control(0),
            scene_volume: value(output, ids::LANE_OUTPUT_CONTROL, 0),
            pan: control(1),
            mute: control(2),
            solo: control(3),
            gate: std::array::from_fn(|index| {
                push.expression
                    .and_then(|expression| {
                        expression
                            .values
                            .get(&(Site::InputControl(row), index as u32))
                    })
                    .copied()
                    .unwrap_or_else(|| value(input, ids::INPUT_GATE_CONTROL, index))
            }),
            path_b_muted: row == pair.slave()
                && junctions.split.is_some()
                && junctions.merge.is_none()
                && lane.output != OutputPort::Empty
                && junctions
                    .mixer
                    .as_ref()
                    .is_some_and(|mixer| mixer.bypass.at(push.scene)),
            input_router: router(plan.routers.lane_input[index])?,
            output_router: router(plan.routers.lane_output[index])?,
        });
    }
    Ok(lanes.try_into().expect("four lanes"))
}

/// One engine gate message as its control request (`0x514e58`: `+2` row, `+4` channel type,
/// body = channel index).
fn gate_request(message: GateMessage) -> ControlRequest {
    let destination = |core: tetrad_engine::DspCore| core.index() as u8;
    match message {
        GateMessage::Enabled { core, value } => ControlRequest::InputGateEnabled {
            destination: destination(core),
            value,
        },
        GateMessage::Source {
            core,
            row,
            channel,
            index,
        } => ControlRequest::InputGateSource {
            destination: destination(core),
            gate: row.index() as u8,
            word: u32::from(channel as u8),
            source: u32::from(index),
        },
        GateMessage::Parameter {
            core,
            row,
            index,
            value,
        } => ControlRequest::InputGateParameter {
            destination: destination(core),
            gate: row.index() as u8,
            parameter: index as i32,
            value_bits: value.to_bits(),
        },
        GateMessage::InputGain { core, row, value } => ControlRequest::InputGateInputGain {
            destination: destination(core),
            gate: row.index() as u8,
            value_bits: value.to_bits(),
        },
    }
}

/// The output calibration gains of an ES9017 board (`0x36d734`, run by the calibration view
/// ctor `0x36d970` at the UI build): for OUT 1..4, SEND 1, 2, HP L, R (`0x56` outputs 0..=7)
/// the U-Boot variable `CAL_*` in dB (`env`, `fw_printenv` `0x47d5b4`; defaults `0xa2fb78`:
/// −2 dB, HP 0 dB; text that is not a number reads 0 dB, as `QString::toFloat`), limited to
/// −4..=0 dB (`0x64b8f8` mode 1, NaN gives 0 dB), sent as the linear gain `10^(dB/20)`.
pub fn output_calibration(env: &BTreeMap<String, String>) -> impl Iterator<Item = Request> + '_ {
    use tetrad_services::bootenv::CalibrationVar as V;
    const DEFAULT_DB: [f32; 8] = [-2.0, -2.0, -2.0, -2.0, -2.0, -2.0, 0.0, 0.0];
    let outputs = [
        V::Out1,
        V::Out2,
        V::Out3,
        V::Out4,
        V::Send1,
        V::Send2,
        V::HpL,
        V::HpR,
    ];
    (0u8..)
        .zip(outputs.into_iter().zip(DEFAULT_DB))
        .map(|(output, (var, default))| {
            let db = env
                .get(var.name())
                .map_or(default, |text| text.trim().parse().unwrap_or(0.0));
            let position = (db + 4.0) / 4.0;
            let position = if position <= 1.0 {
                position.max(0.0)
            } else {
                1.0
            };
            let gain = 10_f32.powf((position * 4.0 - 4.0) * 0.05);
            Request::SetOutputCalibrationGains {
                output,
                gain_bits: gain.to_bits(),
            }
        })
}

/// The block value echo `0x25d51c` of a driver's controller write ([`LinkedEdit::echo`]): a
/// Grid UPDATE with one chain (the row) holding one model (the column) and the value.
pub fn block_echo(slot: Slot, index: u32, value: f32) -> ControlMessage {
    let param = tetrad_preset::Param {
        index: Some(index),
        values: vec![tetrad_preset::SceneValue {
            value: Some(tetrad_preset::ParamValue::Float(value)),
            ..Default::default()
        }],
        ..Default::default()
    };
    let preset = tetrad_preset::Preset {
        chains: vec![tetrad_preset::Chain {
            row: Some(slot.row.index() as u32),
            models: vec![tetrad_preset::Model {
                column: Some(slot.column.index() as u32),
                params: vec![param],
                ..Default::default()
            }],
            ..Default::default()
        }],
        ..Default::default()
    };
    ControlMessage::Grid(GridMessage {
        action: Some(MessageAction::Update.into()),
        preset: Some(RawMessageBytes {
            bytes: preset.to_bytes(),
        }),
        ..Default::default()
    })
}

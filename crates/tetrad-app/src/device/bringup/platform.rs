//! [`Bringup`]: a [`Platform`] decorator between [`Device`](crate::device::Device) and the
//! real platform. It adds the bring-up safety layer without a change in the device logic:
//!
//! - DSP trace (`--trace`): one line per ring send and receive (core, slot, type, length) and
//!   per model-window write (core, address, length). Receive payloads are never written, so
//!   the key replies (`0x24`/`0x25`/`0x27`), the signing replies (`0x11` material, `0x1f`
//!   client id) and the backup stamp key (`0x18`) stay out of the trace. Sends show a 32-byte preview, except the types in
//!   [`REDACTED_SENDS`]. Model-window writes never show bytes.
//! - HAL trace (`--trace`): every HAL command. `--log-input`: every HAL event, platform input
//!   and DIN MIDI message out.
//! - Unmute hold (`--keep-muted`, the mute probe, the `--power stock` teardown): a `0x20` frame
//!   to core 0 or 1 with the mute bit clear is rewritten with the bit set before it reaches the
//!   ring. The session still sees a normal send.
//! - No-persist (default): settings writes go to `<log>/would-write/<file>`, capture storage
//!   is refused. The boot count is never incremented: stock counted this boot (review B16).
//! - Last-line refusals: a HAL write that selects 48 V phantom power without `--allow-phantom`
//!   (B8); every service notice without `--services`, so no Wi-Fi state machine runs (B2); the
//!   power steps `killall tee` and `killall tail` (B16, plan §1.6).
//! - Start-up (B9): each splash phase beats the guard and, until the loop runs, checks the stop
//!   file and the time limit; either ends start-up by unwinding with the [`ExitReason`] as the
//!   payload. A decorator dropped before the loop ran does the start-up exit (`Drop`).
//! - Evidence for `summary.log` and the run log (B5, B6, B10): the fault-count baseline at the
//!   first splash phase after the DSP windows open (after the boot, before the preset load),
//!   marker reads, core-control actions, boot writes per region, the SOC2 buffer and gate,
//!   boot key reply lengths, model starts and diagnostics per core. Types, lengths, flags and
//!   addresses only; never payload bytes.
//! - Logs: notices, `WOULD-WRITE`, `REFUSED`, `DROPPED`, `MARKER`, `BOOT`, `MODEL-START`, health
//!   and `STAGE` lines go to the rotating run log (`run.NNN.log`); the trace to
//!   `trace.NNN.log`; the run header and device facts to `summary.log`.
//!
//! `Bringup` is its own [`Hal`], [`SettingsStorage`] and [`PowerBackend`]: the platform lends
//! those backends by `&mut`, so a decorator that must see every call owns the inner platform
//! and implements them itself. Every provided `Platform` method is forwarded (test
//! `device_bringup::bringup_forwards_every_provided_platform_method`); a new provided method
//! must be forwarded here and added to that test, or the default hides the inner backend.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::fs;
use std::io::{self, Write as _};
use std::panic::resume_unwind;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant, SystemTime};

use tetrad_capture::session::{CaptureStorage, SharcMemory};
use tetrad_dsp_host::drain::DrainMode;
use tetrad_dsp_host::model::{CoreWindows, ModelError, ModelMemory};
use tetrad_dsp_host::session::Clock;
use tetrad_dsp_host::transport::{ControlPacket, DspTransport, RingLayout};
use tetrad_dsp_host::{MemoryError, SharedMemory};
use tetrad_dsp_protocol::address::{Address, Bytes};
use tetrad_dsp_protocol::requests::{Request, RequestFrame};
use tetrad_dsp_protocol::responses::{
    Dispatcher, ResponseBody, ResponseFrame, ResponseType, SlotStart,
};
use tetrad_dsp_protocol::{CoreId, DspCore, MessageType};
use tetrad_hal::analog::Verification;
use tetrad_hal::hal::{Command as HalCommand, Event as HalEvent, Hal, HalError, Query, Reading};
use tetrad_hal::platform::DisplayPanel;
use tetrad_hal::zenjack::{Control, HeadphoneVolume, InputType, Setting, Value};
use tetrad_loader::PhysAddr;
use tetrad_loader::exec::{AccessError, CoreControl, PhysicalMemory};
use tetrad_loader::intent::{CoreAction, LoaderCore};
use tetrad_loader::plan::Variant;
use tetrad_settings::{SettingsFileId, SettingsStorage};
use tetrad_watchdog::power::{PowerBackend, PowerStep, StepStatus};

use super::faults::FaultWatch;
use super::journal::Journal;
use super::{ExitReason, Heartbeat, Host, RunOptions};
use crate::device::Notice;
use crate::device::dsp::DEPLOY_MUTE;
use crate::device::platform::{
    DspFile, DspWindows, Input, Marker, Platform, SharcFirmware, StorageRoots,
};
use crate::device::services::ServiceNotice;

/// Folder under the log folder for settings that no-persist did not write.
pub const WOULD_WRITE: &str = "would-write";
/// Run header, device facts and the `STAGE` line, in the log folder.
pub const SUMMARY: &str = "summary.log";
/// Send payload bytes shown in a trace line.
pub const PREVIEW_BYTES: usize = 32;
/// `Request::MasterMute`: `value = mute | flag << 1`; bit 0 mutes. Inferred from the deploy
/// start `0x876470(dsp, 1, flag)` and standby `(1, 1)`; stage S4a proves it (review B7).
const MUTE_BIT: u32 = 1;

/// Request types whose bodies never reach the trace. Several carry cover names:
/// - model blobs and data transfers: `0x07`, `0x0b`, `0x17`;
/// - Cortex Control reports (ModelRepo payload, key exchange): `0x27`;
/// - the device serial for cloud signing: `0x35` (`SigningMaterial`), `0x4f` (`ClientId`);
/// - account bytes and the licence file: `0x50`, `0x51`;
/// - text arguments `0x42`/`0x43` and the key handshake `0x57`.
pub const REDACTED_SENDS: [MessageType; 11] = [
    MessageType::LoadModel,
    MessageType::ReceiveData,
    MessageType::ReceiveDataEnd,
    MessageType::UsbHidTxToHost,
    MessageType::TunerMuteMessage,
    MessageType::TunerCalibrate,
    MessageType::BulkStatusRead,
    MessageType::GetInputGateParam,
    MessageType::TestLinkPort,
    MessageType::TestSoc2,
    MessageType::DataiMessage,
];

/// The five ring pairs of the message window.
pub(super) const RINGS: [CoreId; 5] = [
    CoreId::Dsp(DspCore::Core0),
    CoreId::Dsp(DspCore::Core1),
    CoreId::Dsp(DspCore::Core2),
    CoreId::Dsp(DspCore::Core3),
    CoreId::Control,
];

/// Boot key vectors whose reply lengths `summary.log` reports (never their bytes).
const KEY_REPLIES: [ResponseType; 3] = [
    ResponseType::Vector24,
    ResponseType::Vector25,
    ResponseType::Vector27,
];

#[derive(Debug, thiserror::Error)]
pub enum BringupError<E: std::error::Error + 'static> {
    #[error(transparent)]
    Platform(E),
    #[error("model window: {0}")]
    Model(#[from] ModelError),
}

/// What `main` knows before the platform is wrapped.
pub struct Launch {
    /// `/opt/neuraldsp/boot_count.txt` read without a write. Bring-up reports it unchanged,
    /// also with `--persist`: stock ZenUI already counted this boot before the stage script
    /// stopped it.
    pub stored_boot_count: u32,
    /// SHA-256 of `/proc/self/exe`, hex.
    pub binary_sha256: Option<String>,
    pub heartbeat: Heartbeat,
    /// The stop file and RSS probes. Start-up polls the stop file too (B9).
    pub host: Box<dyn Host>,
}

/// The unmute hold shared with the ring window. Atomic, like every tap state: the ring window
/// is also read on the DSP drain thread (`tetrad_dsp_host::drain`).
#[derive(Debug, Default)]
pub struct Interlock {
    pub hold: AtomicBool,
    /// `0x20` unmutes rewritten so far.
    pub held: AtomicU64,
}

/// Ring facts the ring tap collects: types, lengths and flags, never payload bytes.
#[derive(Debug, Default)]
pub struct RingStats {
    /// Per protocol core in [`RINGS`] order: diagnostics requests sent and replies received
    /// since the last health line (B10).
    diagnostics: Mutex<[(u32, u32); 5]>,
    /// Reply lengths of [`KEY_REPLIES`] (B6).
    keys: Mutex<[Option<usize>; 3]>,
}

impl RingStats {
    fn count(&self, core: CoreId, answered: bool) {
        if let Some(index) = RINGS.iter().position(|&ring| ring == core) {
            let mut counts = lock(&self.diagnostics);
            if answered {
                counts[index].1 += 1;
            } else {
                counts[index].0 += 1;
            }
        }
    }

    /// `diag core0=<sent>/<answered> …` since the last call.
    pub fn take_diagnostics(&self) -> String {
        let counts = std::mem::take(&mut *lock(&self.diagnostics));
        let mut line = "diag".to_owned();
        for (core, (sent, answered)) in RINGS.iter().zip(counts) {
            let _ = write!(line, " {}={sent}/{answered}", core_name(*core));
        }
        line
    }
}

/// Boot-bus writes per region (B6), shared by [`BootTap`] and the decorator.
#[derive(Debug, Default)]
pub struct BootStats {
    /// `(writes, bytes)` in [`REGIONS`] order.
    writes: [(u64, u64); 6],
    /// The mapped SOC2 buffer `(base, len)`.
    soc2: Option<(u32, u32)>,
}

const REGIONS: [&str; 6] = ["l2", "sharc1-l1", "sharc2-l1", "mmio", "soc2", "other"];

impl BootStats {
    fn record(&mut self, address: PhysAddr, bytes: usize) {
        use tetrad_sys::loader::{L1, L2, MMIO};
        let within = |(base, len): (u32, u32)| address.0.wrapping_sub(base) < len;
        let region = [Some(L2), Some(L1[0]), Some(L1[1]), Some(MMIO), self.soc2]
            .into_iter()
            .position(|range| range.is_some_and(within))
            .unwrap_or(REGIONS.len() - 1);
        let (writes, total) = &mut self.writes[region];
        *writes += 1;
        *total += bytes as u64;
    }
}

/// Facts for `summary.log` that the platform calls reveal (B4, B6).
#[derive(Debug, Default)]
struct BootFacts {
    /// Each marker read so far with its last value.
    markers: Vec<(Marker, bool)>,
    /// `<base> len <len>: mapped` or the refusal.
    soc2_buffer: Option<String>,
    /// `open_soc2`: GPIO 97 on a full-size QC, always set on the mini.
    soc2_gate: Option<bool>,
}

pub struct Bringup<P: Platform> {
    /// `RefCell` only for the `&self` methods of the lent backends (`Hal::cached`,
    /// `SettingsStorage::read`); `&mut self` paths use `get_mut`.
    inner: RefCell<P>,
    clock: P::Clock,
    dir: PathBuf,
    /// Shared with the ring and boot taps for `MODEL-START` and `BOOT` lines.
    run: SharedJournal,
    trace: Option<SharedJournal>,
    trace_commands: bool,
    trace_input: bool,
    persist: bool,
    services: bool,
    allow_phantom: bool,
    time_limit: Option<Duration>,
    /// Settings no-persist has taken: `None` is a removed file.
    would_write: BTreeMap<SettingsFileId, Option<Vec<u8>>>,
    stored_boot_count: u32,
    facts: BootFacts,
    boot: Rc<RefCell<BootStats>>,
    /// The DSP windows were handed out: the start-up exit mutes through a new map of them.
    dsp_mapped: bool,
    /// The fault baseline is due at the next splash phase.
    baseline_due: bool,
    pub(super) host: Box<dyn Host>,
    pub(super) heartbeat: Heartbeat,
    pub(super) interlock: Arc<Interlock>,
    /// Diagnostics counts and key reply lengths from the ring tap.
    pub rings: Arc<RingStats>,
    pub(super) frames: u64,
    /// Slowest inner `present` (framebuffer write) since the last `HEALTH` line.
    pub(super) present_max: Duration,
    pub(super) notices: u64,
    /// Stages from `dsp` on: the core-0 fault count, first read after the DSP boot.
    pub(super) faults: Option<FaultWatch>,
    /// The loop runs: start-up is over and the loop owns the exit.
    pub(super) started: bool,
    /// [`Platform::drill_core0_silence`] still to fire.
    drill_core0: bool,
}

impl<P: Platform> Bringup<P> {
    /// Wraps `inner`. Creates the run log and, for `--trace` or `--log-input`, the trace in
    /// `options.log_dir`, then writes the run header to `summary.log`.
    pub fn new(mut inner: P, options: &RunOptions, launch: Launch) -> io::Result<Self> {
        let dir = options.log_dir.clone().ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "bring-up needs a log folder")
        })?;
        fs::create_dir_all(&dir)?;
        if !options.persist {
            fs::create_dir_all(dir.join(WOULD_WRITE))?;
        }
        let trace = (options.trace || options.log_input)
            .then(|| Journal::create(&dir, "trace"))
            .transpose()?
            .map(|journal| Arc::new(Mutex::new(journal)));
        let cmdline = inner.kernel_cmdline().map_or_else(
            |error| format!("unreadable: {error}"),
            |text| text.lines().next().unwrap_or_default().to_owned(),
        );
        let platform = Self {
            clock: inner.clock(),
            inner: RefCell::new(inner),
            run: Arc::new(Mutex::new(Journal::create(&dir, "run")?)),
            dir,
            trace,
            trace_commands: options.trace,
            trace_input: options.log_input,
            persist: options.persist,
            services: options.services,
            allow_phantom: options.allow_phantom,
            time_limit: options.time_limit,
            would_write: BTreeMap::new(),
            stored_boot_count: launch.stored_boot_count,
            facts: BootFacts::default(),
            boot: Rc::default(),
            dsp_mapped: false,
            baseline_due: false,
            host: launch.host,
            heartbeat: launch.heartbeat,
            interlock: Arc::new(Interlock {
                hold: AtomicBool::new(options.keep_muted || options.mute_probe),
                held: AtomicU64::new(0),
            }),
            rings: Arc::default(),
            frames: 0,
            present_max: Duration::ZERO,
            notices: 0,
            faults: None,
            started: false,
            drill_core0: options.drill_core0,
        };
        platform.summary(&format!(
            "tetrad {} bring-up\nbinary sha256 {}\noptions {options}\ncmdline {cmdline}",
            env!("CARGO_PKG_VERSION"),
            launch.binary_sha256.as_deref().unwrap_or("unknown"),
        ))?;
        Ok(platform)
    }

    /// Appends to `summary.log`: the run header, the device facts and the `STAGE` line.
    pub(super) fn summary(&self, text: &str) -> io::Result<()> {
        append_summary(&self.dir, text)
    }

    /// One run-log line, stamped with device time.
    pub(super) fn note(&mut self, line: &str) {
        journal_line(&self.run, self.clock.now(), line);
    }

    pub(super) fn beat(&self) {
        self.heartbeat.beat(self.clock.now());
    }

    /// `FAULTS <when> faults core0=<n>`, or `?` without SHARC memory.
    pub(super) fn fault_line(&mut self, when: &str) {
        let field = match self.faults.as_mut() {
            Some(watch) => watch.sample(),
            None => "faults core0=? (no SHARC memory)".into(),
        };
        self.note(&format!("FAULTS {when} {field}"));
    }

    /// Boot evidence lines for `summary.log` (B4, B6): markers and the start kind, boot-bus
    /// writes per region, the SOC2 buffer and gate, and the boot key reply lengths.
    pub(super) fn boot_summary(&self) -> String {
        let mut text = "markers".to_owned();
        for (marker, value) in &self.facts.markers {
            let _ = write!(text, " {marker:?}={value}");
        }
        let start = self
            .facts
            .markers
            .iter()
            .find(|(marker, _)| *marker == Marker::SharcsStarted)
            .map_or(
                "unknown",
                |&(_, restart)| if restart { "Restart" } else { "First" },
            );
        let _ = write!(text, "\nboot start={start} writes");
        for (name, (writes, bytes)) in REGIONS.iter().zip(self.boot.borrow().writes) {
            let _ = write!(text, " {name}={writes}/{bytes}B");
        }
        let _ = write!(
            text,
            "\nsoc2 buffer {}\nsoc2 gate {}\nkey replies",
            self.facts.soc2_buffer.as_deref().unwrap_or("not reported"),
            self.facts
                .soc2_gate
                .map_or("not read", |high| if high { "high" } else { "low" }),
        );
        for (kind, len) in KEY_REPLIES.iter().zip(*lock(&self.rings.keys)) {
            match len {
                Some(len) => {
                    let _ = write!(text, " {:#04x}={len}B", *kind as u8);
                }
                None => {
                    let _ = write!(text, " {:#04x}=absent", *kind as u8);
                }
            }
        }
        text
    }

    fn trace_line(&self, line: &str) {
        trace_to(self.trace.as_ref(), self.clock.now(), line);
    }

    /// The decorated core window: same core, base and external region, writes traced.
    fn tap_model(
        &self,
        model: ModelMemory<P::Window>,
    ) -> Result<ModelMemory<TapWindow<P>>, ModelError> {
        let (core, base) = (model.core(), model.base());
        let kind = |base: Address<Bytes>| TapKind::Model {
            core,
            base: base.value(),
        };
        let (memory, external) = model.into_parts();
        let tapped = ModelMemory::new(core, base, self.tap(memory, kind(base)))?;
        match external {
            Some((at, memory)) => tapped.with_external(at, self.tap(memory, kind(at))),
            None => Ok(tapped),
        }
    }

    fn tap(&self, inner: P::Window, kind: TapKind) -> TapWindow<P> {
        Tap {
            inner,
            kind,
            clock: self.clock.clone(),
            trace: self.trace.clone().filter(|_| self.trace_commands),
            interlock: self.interlock.clone(),
        }
    }

    /// `0x20` value 1 to cores 0 and 1 through a new map of the ring window. The session that
    /// owned the old map is gone; the ring indices live in DSP memory, so a new transport
    /// continues them.
    fn mute_rings(&mut self) -> Result<(), String> {
        let windows = self
            .inner
            .get_mut()
            .dsp_windows()
            .map_err(|error| error.to_string())?;
        let mut transport =
            DspTransport::new(windows.messages).map_err(|error| error.to_string())?;
        for core in [DspCore::Core0, DspCore::Core1] {
            let frame = RequestFrame::new(Request::MasterMute {
                core,
                value: DEPLOY_MUTE,
            })
            .map_err(|error| error.to_string())?;
            transport
                .send_control(ControlPacket::Request(&frame))
                .map_err(|error| error.to_string())?;
        }
        Ok(())
    }
}

/// Appends one block to `<dir>/summary.log`.
pub(super) fn append_summary(dir: &Path, text: &str) -> io::Result<()> {
    fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join(SUMMARY))?
        .write_all(format!("{text}\n").as_bytes())
}

/// The where clause: the inner platform's drain also serves the tapped window. On the device
/// (`StockThread`) that needs `TapWindow<P>: Send`, which holds because the tap state is atomic
/// or locked; the test sims drain on the main loop.
impl<P: Platform> Platform for Bringup<P>
where
    P::RingDrain: DrainMode<TapWindow<P>>,
{
    type Error = BringupError<P::Error>;
    type Clock = P::Clock;
    type BootBus = BootTap<P::BootBus, P::Clock>;
    type Window = TapWindow<P>;
    type RingDrain = P::RingDrain;
    type Hal = Self;
    type Settings = Self;
    type Splash = P::Splash;
    type Power = Self;
    type Services = P::Services;

    fn clock(&self) -> P::Clock {
        self.clock.clone()
    }
    fn kernel_cmdline(&mut self) -> Result<String, Self::Error> {
        self.inner
            .get_mut()
            .kernel_cmdline()
            .map_err(BringupError::Platform)
    }
    /// Logs each marker's first value and each change (`NoPoll` is read every second).
    fn marker(&mut self, marker: Marker) -> Result<bool, Self::Error> {
        let value = self
            .inner
            .get_mut()
            .marker(marker)
            .map_err(BringupError::Platform)?;
        match self
            .facts
            .markers
            .iter_mut()
            .find(|(seen, _)| *seen == marker)
        {
            Some((_, last)) if *last == value => return Ok(value),
            Some((_, last)) => *last = value,
            None => self.facts.markers.push((marker, value)),
        }
        self.note(&format!("MARKER {marker:?}={value}"));
        Ok(value)
    }
    fn set_marker(&mut self, marker: Marker) -> Result<(), Self::Error> {
        self.note(&format!("MARKER set {marker:?}"));
        self.inner
            .get_mut()
            .set_marker(marker)
            .map_err(BringupError::Platform)
    }
    /// Always the stored count, also with `--persist` (B16).
    fn boot_count(&mut self) -> Result<u32, Self::Error> {
        Ok(self.stored_boot_count)
    }
    /// Each start-up phase: beats the guard. Until the loop runs it also ends start-up on the
    /// stop file or the time limit (B9) and, once the DSP windows are open, takes the
    /// fault-count baseline before the preset load (B5).
    fn splash(&mut self) -> &mut P::Splash {
        self.beat();
        if !self.started {
            let stop = if self.host.stop_requested() {
                Some(ExitReason::StopFile)
            } else if self
                .time_limit
                .is_some_and(|limit| self.clock.now() >= limit)
            {
                Some(ExitReason::TimeLimit)
            } else {
                None
            };
            if let Some(reason) = stop {
                self.note(&format!("EXIT {reason} during start-up"));
                resume_unwind(Box::new(reason));
            }
            if std::mem::take(&mut self.baseline_due) {
                self.faults = self.inner.get_mut().sharc_memory().map(FaultWatch::new);
                self.fault_line("boot");
            }
        }
        self.inner.get_mut().splash()
    }
    fn power(&mut self) -> &mut Self {
        self
    }
    fn settings(&mut self) -> &mut Self {
        self
    }
    fn storage(&self) -> StorageRoots {
        self.inner.borrow().storage()
    }
    fn hal(&mut self) -> &mut Self {
        self
    }
    fn services(&mut self) -> &mut P::Services {
        self.inner.get_mut().services()
    }
    fn firmware(&mut self, variant: Variant) -> Result<SharcFirmware, Self::Error> {
        self.inner
            .get_mut()
            .firmware(variant)
            .map_err(BringupError::Platform)
    }
    fn boot_bus(&mut self) -> Result<Self::BootBus, Self::Error> {
        let inner = self
            .inner
            .get_mut()
            .boot_bus()
            .map_err(BringupError::Platform)?;
        Ok(BootTap {
            inner,
            stats: self.boot.clone(),
            run: self.run.clone(),
            clock: self.clock.clone(),
        })
    }
    /// Logs the DSP-given buffer before the inner platform checks and maps it (B4).
    fn map_soc2_buffer(
        &mut self,
        bus: &mut Self::BootBus,
        buffer: PhysAddr,
        bytes: u32,
    ) -> Result<(), Self::Error> {
        self.note(&format!("BOOT soc2 buffer {buffer} len {bytes:#x}"));
        let mapped = self
            .inner
            .get_mut()
            .map_soc2_buffer(&mut bus.inner, buffer, bytes);
        let outcome = match &mapped {
            Ok(()) => {
                self.boot.borrow_mut().soc2 = Some((buffer.0, bytes));
                "mapped".to_owned()
            }
            Err(error) => format!("refused: {error}"),
        };
        self.note(&format!("BOOT soc2 buffer {outcome}"));
        self.facts.soc2_buffer = Some(format!("{buffer} len {bytes:#x}: {outcome}"));
        mapped.map_err(BringupError::Platform)
    }
    fn open_soc2(&mut self) -> Result<bool, Self::Error> {
        let high = self
            .inner
            .get_mut()
            .open_soc2()
            .map_err(BringupError::Platform)?;
        self.note(&format!(
            "BOOT soc2 gate {}",
            if high { "high" } else { "low" }
        ));
        self.facts.soc2_gate = Some(high);
        Ok(high)
    }
    fn drill_core0_silence(&mut self) -> bool {
        let drill = std::mem::take(&mut self.drill_core0);
        if drill {
            self.note("DRILL core 0 reported silent once after the handshake");
        }
        drill
    }
    fn dsp_windows(&mut self) -> Result<DspWindows<Self::Window>, Self::Error> {
        let windows = self
            .inner
            .get_mut()
            .dsp_windows()
            .map_err(BringupError::Platform)?;
        self.dsp_mapped = true;
        self.baseline_due = true;
        let (core0, core1) = windows.cores.into_inner();
        let rings = TapKind::Rings(Box::new(RefCell::new(Rings {
            sends: Default::default(),
            receives: Default::default(),
            stats: self.rings.clone(),
            run: self.run.clone(),
        })));
        Ok(DspWindows {
            messages: self.tap(windows.messages, rings),
            cores: CoreWindows::new(self.tap_model(core0)?, self.tap_model(core1)?)?,
        })
    }
    fn dsp_file(&mut self, file: DspFile<'_>) -> Result<Vec<u8>, Self::Error> {
        self.inner
            .get_mut()
            .dsp_file(file)
            .map_err(BringupError::Platform)
    }
    /// Without `--services` every service notice is dropped here (B2): a supplicant event
    /// would otherwise run the Wi-Fi state machine (`DISABLE_NETWORK`, `SAVE_CONFIG`).
    fn poll_input(&mut self, out: &mut Vec<Input>) -> Result<(), Self::Error> {
        let first = out.len();
        let polled = self.inner.get_mut().poll_input(out);
        let inputs: Vec<Input> = out.drain(first..).collect();
        for input in inputs {
            match input {
                Input::Service(notice) if !self.services => {
                    let kind = service_kind(&notice);
                    self.note(&format!("DROPPED {kind} without --services"));
                }
                input => {
                    if self.trace_input {
                        match &input {
                            // Service results can hold network and account data.
                            Input::Service(_) => self.trace_line("IN service notice"),
                            input => self.trace_line(&format!("IN {input:?}")),
                        }
                    }
                    out.push(input);
                }
            }
        }
        polled.map_err(BringupError::Platform)
    }
    fn midi_write(&mut self, message: &[u8]) -> Result<(), Self::Error> {
        if self.trace_input {
            self.trace_line(&format!("OUT MIDI {}", hex(message)));
        }
        self.inner
            .get_mut()
            .midi_write(message)
            .map_err(BringupError::Platform)
    }
    fn present(&mut self, rgb: &[u8]) -> Result<(), Self::Error> {
        self.present_rows(rgb, 0..tetrad_ui::HEIGHT)
    }
    fn present_rows(&mut self, rgb: &[u8], rows: std::ops::Range<u32>) -> Result<(), Self::Error> {
        self.frames += 1;
        let start = Instant::now();
        let presented = self.inner.get_mut().present_rows(rgb, rows);
        self.present_max = self.present_max.max(start.elapsed());
        presented.map_err(BringupError::Platform)
    }
    /// Notices go to the rotating run log only: `stderr.log` has no size bound.
    fn log(&mut self, notice: &Notice) {
        self.notices += 1;
        self.note(&format!("NOTICE {:?}: {}", notice.subsystem, notice.detail));
    }
    fn random_u32(&mut self) -> Option<u32> {
        self.inner.get_mut().random_u32()
    }
    fn capture_storage(&mut self) -> Option<Box<dyn CaptureStorage>> {
        if self.persist {
            return self.inner.get_mut().capture_storage();
        }
        self.note("REFUSED capture storage (no-persist)");
        None
    }
    fn sharc_memory(&mut self) -> Option<Box<dyn SharcMemory>> {
        self.inner.get_mut().sharc_memory()
    }
    fn media_cipher(&mut self) -> Option<tetrad_media::factory::DeviceCipher> {
        self.inner.get_mut().media_cipher()
    }
    fn wall_clock(&mut self) -> Option<SystemTime> {
        self.inner.get_mut().wall_clock()
    }
    fn display_panel(&mut self) -> Option<DisplayPanel> {
        self.inner.get_mut().display_panel()
    }
    fn splash_running(&mut self) -> bool {
        self.inner.get_mut().splash_running()
    }
    fn disk_space(&mut self) -> Option<tetrad_sys::fs::FsSpace> {
        self.inner.get_mut().disk_space()
    }
    fn forget_display(&mut self) {
        self.inner.get_mut().forget_display()
    }
}

impl<P: Platform> Drop for Bringup<P> {
    /// The start-up exit (B9): start-up ended early (stop file, time limit, panic or error), so
    /// the device and its DSP session are gone. Best effort, as the test exit: `0x20` value 1
    /// to cores 0 and 1 once the DSP windows were open, then headphones 0. After the loop
    /// started, the loop's test exit owns this.
    fn drop(&mut self) {
        if self.started {
            return;
        }
        self.note("EXIT start-up ended before the loop: start-up exit");
        if self.dsp_mapped {
            let line = match self.mute_rings() {
                Ok(()) => "EXIT start-up 0x20 mute sent to cores 0 and 1".to_owned(),
                Err(error) => format!("EXIT start-up 0x20 mute failed: {error}"),
            };
            self.note(&line);
        } else {
            self.note("EXIT no DSP: 0x20 mute not sent");
        }
        // `SetHeadPhoneVolume(0.0, …)` as `PowerStep::MuteHeadphones`.
        let muted = Setting::new(
            Control::HeadphoneVolume,
            Value::HeadphoneVolume(HeadphoneVolume::Muted),
        )
        .map_err(HalError::from)
        .and_then(|setting| {
            self.inner
                .get_mut()
                .hal()
                .apply(HalCommand::Set(setting, Verification::Unverified))
        });
        let line = match muted {
            Ok(()) => "EXIT start-up headphones muted".to_owned(),
            Err(error) => format!("EXIT start-up headphone mute failed: {error}"),
        };
        self.note(&line);
    }
}

impl<P: Platform> Hal for Bringup<P> {
    /// The last phantom check (B8): `Device::execute` refuses every phantom effect first; a
    /// path that reaches the HAL directly is refused here.
    fn apply(&mut self, command: HalCommand) -> Result<(), HalError> {
        // Review bring-up safety 2 R7: no tetrad-app path emits a power ioctl. The test exit and
        // the stock teardown (`PowerStep::HalRestart`/`HalShutdown` through `PowerBackend`) do
        // not come here.
        if matches!(command, HalCommand::Power(_)) {
            self.note(&format!(
                "REFUSED HAL {command:?}: a power command in bring-up"
            ));
            return Err(HalError::Refused("a power command in bring-up"));
        }
        if !self.allow_phantom
            && let HalCommand::Set(setting, _) = command
            && setting.value() == Value::InputType(InputType::XlrPhantom)
        {
            self.note(&format!(
                "REFUSED HAL {command:?}: 48 V phantom power without --allow-phantom"
            ));
            return Err(HalError::Refused(
                "48 V phantom power without --allow-phantom",
            ));
        }
        let applied = self.inner.get_mut().hal().apply(command);
        if self.trace_commands {
            let outcome = applied
                .as_ref()
                .map_or_else(ToString::to_string, |()| "ok".into());
            self.trace_line(&format!("HAL {command:?} {outcome}"));
        }
        applied
    }
    fn read(&mut self, query: Query) -> Result<Reading, HalError> {
        self.inner.get_mut().hal().read(query)
    }
    fn cached(&self, control: Control) -> Option<Value> {
        self.inner.borrow_mut().hal().cached(control)
    }
    fn poll_events(&mut self, out: &mut Vec<HalEvent>) -> Result<(), HalError> {
        let first = out.len();
        let polled = self.inner.get_mut().hal().poll_events(out);
        if self.trace_input {
            for event in &out[first..] {
                self.trace_line(&format!("IN HAL {event:?}"));
            }
        }
        polled
    }
}

impl<P: Platform> PowerBackend for Bringup<P> {
    type Error = String;

    /// Logs each system power step (S10 evidence). `killall tee` and `killall tail` are refused
    /// (plan §1.6, review B16): they end processes this run does not own.
    fn perform(&mut self, step: PowerStep) -> Result<StepStatus, String> {
        if matches!(
            step,
            PowerStep::StopLogPipelines | PowerStep::StopLogFollower
        ) {
            self.note(&format!("REFUSED power step {step:?} (killall, plan §1.6)"));
            return Ok(StepStatus::Failure);
        }
        let done = self
            .inner
            .get_mut()
            .power()
            .perform(step)
            .map_err(|error| error.to_string());
        self.note(&format!("POWER {step:?} {done:?}"));
        done
    }
}

impl<P: Platform> SettingsStorage for Bringup<P> {
    fn read(&self, file: SettingsFileId) -> io::Result<Option<Vec<u8>>> {
        match self.would_write.get(&file) {
            Some(taken) => Ok(taken.clone()),
            None => self.inner.borrow_mut().settings().read(file),
        }
    }

    /// No-persist: the file goes to the would-write folder, except the account and licence
    /// files (credentials), which are only measured.
    fn write(&mut self, file: SettingsFileId, bytes: &[u8]) -> io::Result<()> {
        if self.persist {
            return self.inner.get_mut().settings().write(file, bytes);
        }
        let same = self.inner.get_mut().settings().read(file)?.as_deref() == Some(bytes);
        let name = file_name(file);
        let target = match file {
            SettingsFileId::Account | SettingsFileId::PluginLicenses => {
                "content withheld (credentials)".to_owned()
            }
            SettingsFileId::Zenjack | SettingsFileId::Permanent => {
                fs::write(self.dir.join(WOULD_WRITE).join(name), bytes)?;
                format!("{WOULD_WRITE}/{name}")
            }
        };
        self.note(&format!(
            "WOULD-WRITE {name} {} bytes, same as the original: {}, {target}",
            bytes.len(),
            if same { "yes" } else { "no" }
        ));
        self.would_write.insert(file, Some(bytes.to_vec()));
        Ok(())
    }

    fn remove(&mut self, file: SettingsFileId) -> io::Result<()> {
        if self.persist {
            return self.inner.get_mut().settings().remove(file);
        }
        self.note(&format!("WOULD-REMOVE {}", file_name(file)));
        self.would_write.insert(file, None);
        Ok(())
    }
}

/// The boot bus under the decorator (B6): counts writes per region for `summary.log` and logs
/// each core-control action (`BOOT control …`) to the run log. Reads and waits pass through.
pub struct BootTap<B, C> {
    inner: B,
    stats: Rc<RefCell<BootStats>>,
    run: SharedJournal,
    clock: C,
}

impl<B: PhysicalMemory, C> PhysicalMemory for BootTap<B, C> {
    fn read_u32(&mut self, address: PhysAddr) -> Result<u32, AccessError> {
        self.inner.read_u32(address)
    }
    fn write_u32(&mut self, address: PhysAddr, value: u32) -> Result<(), AccessError> {
        self.stats.borrow_mut().record(address, 4);
        self.inner.write_u32(address, value)
    }
    fn write_u16(&mut self, address: PhysAddr, value: u16) -> Result<(), AccessError> {
        self.stats.borrow_mut().record(address, 2);
        self.inner.write_u16(address, value)
    }
    fn write_bytes(&mut self, address: PhysAddr, bytes: &[u8]) -> Result<(), AccessError> {
        self.stats.borrow_mut().record(address, bytes.len());
        self.inner.write_bytes(address, bytes)
    }
    fn now(&self) -> Duration {
        self.inner.now()
    }
    fn delay(&mut self, duration: Duration) {
        self.inner.delay(duration);
    }
}

impl<B: CoreControl, C: Clock> CoreControl for BootTap<B, C> {
    fn control(&mut self, core: LoaderCore, action: CoreAction) -> Result<(), AccessError> {
        let done = self.inner.control(core, action);
        let line = format!("BOOT control {core:?} {action:?} {done:?}");
        journal_line(&self.run, self.clock.now(), &line);
        done
    }
}

/// The DSP window type of [`Bringup`] over `P`.
pub type TapWindow<P> = Tap<<P as Platform>::Window, <P as Platform>::Clock>;

/// One DSP window under the decorator: the ring window or a core 0/1 model window.
pub struct Tap<W, C> {
    inner: W,
    kind: TapKind,
    clock: C,
    /// Present only with `--trace`.
    trace: Option<SharedJournal>,
    interlock: Arc<Interlock>,
}

enum TapKind {
    /// The message window. `RefCell`: receive payloads are read through `&self`.
    Rings(Box<RefCell<Rings>>),
    Model {
        core: DspCore,
        base: u32,
    },
}

/// Per ring (index in [`RINGS`]): the send written but not yet published, and the receive
/// read but not yet released; the shared ring facts and the run log for `MODEL-START`.
struct Rings {
    sends: [Option<Send>; 5],
    receives: [Option<(u8, usize)>; 5],
    stats: Arc<RingStats>,
    run: SharedJournal,
}

struct Send {
    message_type: u8,
    len: usize,
    /// Hex of the first bytes; `None` for [`REDACTED_SENDS`].
    preview: Option<String>,
    wait: Option<bool>,
    held: bool,
}

impl<W: SharedMemory, C: Clock> Tap<W, C> {
    fn line(&self, line: &str) {
        trace_to(self.trace.as_ref(), self.clock.now(), line);
    }

    /// Follows the stock publish order (payload, length, wait flag, producer index) and the
    /// receive release (consumer index), holds unmutes in the payload and counts diagnostics
    /// requests per target core.
    fn write_ring(&mut self, offset: usize, data: &[u8]) -> Result<(), MemoryError> {
        let TapKind::Rings(rings) = &self.kind else {
            return self.inner.write(offset, data);
        };
        for (index, core) in RINGS.into_iter().enumerate() {
            let out = RingLayout::outgoing(core);
            if in_payload(out, offset) && !data.is_empty() {
                let held = (self.interlock.hold.load(Ordering::Relaxed)
                    && data[0] == MessageType::MasterMute as u8)
                    .then(|| held_unmute(data))
                    .flatten();
                let bytes = held.as_ref().map_or(data, RequestFrame::as_bytes);
                let mut rings = rings.borrow_mut();
                if bytes[0] == MessageType::DiagnosticsRequest as u8
                    && let Ok(frame) = RequestFrame::decode(bytes)
                    && let Request::DiagnosticsRequest { core } = frame.request()
                {
                    rings.stats.count(core, false);
                }
                rings.sends[index] = Some(Send {
                    message_type: bytes[0],
                    len: bytes.len(),
                    preview: preview(bytes),
                    wait: None,
                    held: held.is_some(),
                });
                if held.is_some() {
                    self.interlock.held.fetch_add(1, Ordering::Relaxed);
                }
                return self.inner.write(offset, bytes);
            }
            if let Some(flags) = out.wait_flags_offset
                && (flags..flags + usize::from(out.slot_count)).contains(&offset)
                && let Some(send) = rings.borrow_mut().sends[index].as_mut()
            {
                send.wait = data.first().map(|&flag| flag != 0);
            }
            if offset == out.producer_offset
                && let Some(send) = rings.borrow_mut().sends[index].take()
            {
                self.line(&send_line(core, data, &send));
            }
            let incoming = RingLayout::incoming(core);
            if offset == incoming.consumer_offset
                && let Some((kind, len)) = rings.borrow_mut().receives[index].take()
            {
                let name = ResponseType::try_from(kind)
                    .map_or_else(|_| "?".into(), |kind| format!("{kind:?}"));
                let slot = data.first().copied().unwrap_or_default();
                self.line(&format!(
                    "RECV ring={core:?} slot={slot} type={kind:#04x} {name} len={len}"
                ));
            }
        }
        self.inner.write(offset, data)
    }

    /// Facts of one received frame: key reply lengths, diagnostics replies per core and model
    /// starts. Flags and lengths only; a frame that does not decode is skipped.
    fn on_receive(&self, rings: &Rings, ring: CoreId, bytes: &[u8]) {
        let Ok(frame) = ResponseFrame::decode(bytes) else {
            return;
        };
        let Some(kind) = frame.response_type() else {
            return;
        };
        if let Some(index) = KEY_REPLIES.iter().position(|&key| key == kind) {
            let mut keys = lock(&rings.stats.keys);
            keys[index] = Some(bytes.len());
        }
        let start = |core: u32, slot: u32, started: bool| {
            let line = format!(
                "MODEL-START ring={} core={core} slot={slot} started={started}",
                core_name(ring)
            );
            journal_line(&rings.run, self.clock.now(), &line);
        };
        match *frame.body() {
            ResponseBody::Diagnostics(_) => {
                let selector = frame.header().selector;
                let core = Dispatcher::for_ring(ring).diagnostics_core(selector);
                rings.stats.count(CoreId::Dsp(core), true);
            }
            ResponseBody::Soc2ArmDiagnostics(_) => rings.stats.count(CoreId::Control, true),
            ResponseBody::ModelStart(record) => start(record.core, record.slot, record.status != 0),
            ResponseBody::BulkModelStart(record) => {
                for (slot, outcome) in record.slots() {
                    start(record.core, slot, outcome == SlotStart::Started);
                }
            }
            _ => {}
        }
    }
}

impl<W: SharedMemory, C: Clock> SharedMemory for Tap<W, C> {
    fn len(&self) -> usize {
        self.inner.len()
    }
    fn read(&self, offset: usize, buf: &mut [u8]) -> Result<(), MemoryError> {
        self.inner.read(offset, buf)?;
        if let TapKind::Rings(rings) = &self.kind
            && let Some(&kind) = buf.first()
            && let Some(index) = RINGS
                .iter()
                .position(|&core| in_payload(RingLayout::incoming(core), offset))
        {
            let mut rings = rings.borrow_mut();
            rings.receives[index] = Some((kind, buf.len()));
            self.on_receive(&rings, RINGS[index], buf);
        }
        Ok(())
    }
    fn write(&mut self, offset: usize, data: &[u8]) -> Result<(), MemoryError> {
        match self.kind {
            TapKind::Model { core, base } => {
                self.line(&format!(
                    "MODEL core={core:?} address={:#x} len={}",
                    u64::from(base) + offset as u64,
                    data.len()
                ));
                self.inner.write(offset, data)
            }
            TapKind::Rings(_) => self.write_ring(offset, data),
        }
    }
    fn read_u32(&self, offset: usize) -> Result<u32, MemoryError> {
        self.inner.read_u32(offset)
    }
    fn write_u32(&mut self, offset: usize, value: u32) -> Result<(), MemoryError> {
        self.inner.write_u32(offset, value)
    }
    fn acquire_barrier(&mut self) {
        self.inner.acquire_barrier();
    }
    fn publish_barrier(&mut self) {
        self.inner.publish_barrier();
    }
}

/// The frame with the mute bit set, when `bytes` is a `0x20` frame with it clear.
fn held_unmute(bytes: &[u8]) -> Option<RequestFrame> {
    match RequestFrame::decode(bytes).ok()?.request() {
        Request::MasterMute { core, value } if value & MUTE_BIT == 0 => {
            RequestFrame::new(Request::MasterMute {
                core,
                value: value | MUTE_BIT,
            })
            .ok()
        }
        _ => None,
    }
}

fn send_line(core: CoreId, producer: &[u8], send: &Send) -> String {
    let name = MessageType::try_from(send.message_type)
        .map_or_else(|_| "?".into(), |kind| format!("{kind:?}"));
    let mut line = format!(
        "SEND core={core:?} slot={} type={:#04x} {name} len={}",
        producer.first().copied().unwrap_or_default(),
        send.message_type,
        send.len
    );
    if let Some(wait) = send.wait {
        let _ = write!(line, " wait={}", u8::from(wait));
    }
    if send.held {
        line.push_str(" UNMUTE-HELD");
    }
    match &send.preview {
        Some(bytes) => {
            let _ = write!(line, " bytes={bytes}");
        }
        None => line.push_str(" bytes=redacted"),
    }
    line
}

fn preview(bytes: &[u8]) -> Option<String> {
    let redacted = REDACTED_SENDS
        .iter()
        .any(|&kind| bytes.first() == Some(&(kind as u8)));
    (!redacted).then(|| hex(&bytes[..bytes.len().min(PREVIEW_BYTES)]))
}

fn in_payload(layout: RingLayout, offset: usize) -> bool {
    let end = layout.payload_offset + usize::from(layout.slot_count) * layout.slot_bytes;
    (layout.payload_offset..end).contains(&offset)
}

/// `core0`..`core3` for the DSP cores, `control` for the SOC2 control core.
fn core_name(core: CoreId) -> String {
    match core {
        CoreId::Dsp(core) => format!("core{}", core as u8),
        CoreId::Control => "control".into(),
    }
}

/// The variant only: service notices can hold network and account data.
fn service_kind(notice: &ServiceNotice) -> &'static str {
    match notice {
        ServiceNotice::Supplicant(_) => "supplicant event",
        ServiceNotice::Dhcp { .. } => "DHCP result",
        ServiceNotice::Internet(_) => "internet probe result",
        ServiceNotice::Download(_) => "update download result",
        ServiceNotice::Install(_) => "update install result",
        ServiceNotice::Cloud(_) => "cloud reply",
        ServiceNotice::Backup(_) => "backup result",
        ServiceNotice::TetradChecked(_) => "Tetrad update check result",
        ServiceNotice::TetradInstalled(_) => "Tetrad update install result",
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().fold(String::new(), |mut text, byte| {
        let _ = write!(text, "{byte:02x}");
        text
    })
}

fn file_name(file: SettingsFileId) -> &'static str {
    let path = file.path();
    path.rsplit_once('/').map_or(path, |(_, name)| name)
}

/// A run or trace log shared by the decorator and its taps.
type SharedJournal = Arc<Mutex<Journal>>;

/// A poisoned lock still logs: the lines matter most after a panic.
fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

fn journal_line(journal: &Mutex<Journal>, at: Duration, line: &str) {
    lock(journal).line(&format!("{} {line}", stamp(at)));
}

fn trace_to(trace: Option<&SharedJournal>, at: Duration, line: &str) {
    if let Some(trace) = trace {
        journal_line(trace, at, line);
    }
}

/// Device time as seconds with milliseconds.
pub(super) fn stamp(at: Duration) -> String {
    format!("{:>9.3}", at.as_secs_f64())
}

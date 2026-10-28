//! Bring-up safety contracts (`tetrad bringup`, w4-bringup-plan §4 P9 and the fix round of
//! review-bringup-safety-20261011) on a simulated platform: the time limit, the stop file and
//! a panic each end in the test exit (headphones 0, no system power step), in start-up too;
//! no-persist writes nothing to the real files; the trace never holds key, signing or blob
//! bytes; the unmute hold; the guard verdict; the read-only fault count check and its baseline
//! before the first deploy; the last-line refusals (phantom, Wi-Fi, `killall`); the forwarded
//! platform methods; the boot evidence in `summary.log`. Probe-stage runs need no SHARC; DSP
//! runs use the scripted QC of `tests/common`. All bytes are invented.

mod common;

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, SystemTime};

use tetrad_app::device::bringup::{
    Bringup, Exit, ExitReason, FAULT_COUNT_ADDRESS, Heartbeat, Host, Launch, PANIC_EXIT, PowerMode,
    RunOptions, SUMMARY, Stage, Stall, WOULD_WRITE,
};
use tetrad_app::device::platform::{
    DspFile, DspWindows, Input, Marker, Platform, ServiceBackends, ServiceIo, SharcFirmware,
    StorageRoots,
};
use tetrad_app::device::services::ServiceNotice;
use tetrad_app::device::{Action, Device, Event, PowerAction};
use tetrad_capture::session::{CaptureStorage, FsStorage, SharcMemory};
use tetrad_dsp_host::model::{CoreWindows, ModelMemory};
use tetrad_dsp_host::session::Clock;
use tetrad_dsp_host::transport::{ControlPacket, DspTransport, MESSAGE_MEMORY_BYTES, RingLayout};
use tetrad_dsp_host::{MemoryError, SharedMemory};
use tetrad_dsp_protocol::address::Address;
use tetrad_dsp_protocol::fragment::{Fragmenter, MessageMetadata};
use tetrad_dsp_protocol::requests::{Request, RequestFrame};
use tetrad_dsp_protocol::responses::{ResponseType, encode};
use tetrad_dsp_protocol::{
    CodecLimits, CoreId, DeploymentTarget, DspCore, MessageType, ModuleSlot,
};
use tetrad_hal::analog::Verification;
use tetrad_hal::hal::{
    Command as HalCommand, Event as HalEvent, Hal, HalError, Query, Reading, RecordingHal,
};
use tetrad_hal::platform::DisplayPanel;
use tetrad_hal::requests::{Brightness, DisplayCommand};
use tetrad_hal::zenjack::{Control, HeadphoneVolume, Input as Jack, InputType, Setting, Value};
use tetrad_loader::PhysAddr;
use tetrad_loader::fake::RecordingBus;
use tetrad_loader::plan::Variant;
use tetrad_media::factory::{DeviceCipher, FactoryIrSeed, SEED_LEN};
use tetrad_services::fake::{MemFiles, ScriptedHttp, ScriptedNet, ScriptedRunner};
use tetrad_services::wifi::{LinkError, SupplicantLink};
use tetrad_settings::io::ConnectorId;
use tetrad_settings::{SettingsFileId, SettingsStorage};
use tetrad_storage::media::MediaRoot;
use tetrad_watchdog::power::{PowerBackend, PowerStep, StepStatus};
use tetrad_watchdog::splash::{SplashCommand, SplashQueue};

/// Base of the simulated core 0/1 model windows (the stock L1 window offset).
const MODEL_BASE: u32 = 0x24_0000;

/// Recovered stock DDL at schema 410.1 (as `device_boot.rs`): a real `zenui.db` to keep unchanged.
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

/// What the simulated hardware and storage saw.
#[derive(Debug, Default)]
struct Log {
    hal: Vec<HalCommand>,
    power: Vec<PowerStep>,
    settings_writes: Vec<SettingsFileId>,
    boot_count_reads: u32,
    /// `Platform::sharc_memory` accesses: (address, length).
    sharc_reads: Vec<(u32, usize)>,
    sharc_writes: Vec<(u32, usize)>,
    /// `wpa_supplicant` control requests.
    supplicant: Vec<String>,
}

/// The answer of [`Sim::random_u32`].
const RANDOM: u32 = 0x5eed_1234;

fn wall_clock() -> SystemTime {
    SystemTime::UNIX_EPOCH + Duration::from_secs(1_760_000_000)
}

#[derive(Clone, Default)]
struct SimClock(Rc<RefCell<Duration>>);

impl Clock for SimClock {
    fn now(&self) -> Duration {
        *self.0.borrow()
    }
    fn sleep(&mut self, duration: Duration) {
        *self.0.borrow_mut() += duration;
    }
}

/// One DSP window, shared so a test can play the SHARC side.
#[derive(Clone)]
struct Mem(Rc<RefCell<Vec<u8>>>);

impl Mem {
    fn new(bytes: usize) -> Self {
        Self(Rc::new(RefCell::new(vec![0; bytes])))
    }
}

impl SharedMemory for Mem {
    fn len(&self) -> usize {
        self.0.borrow().len()
    }
    fn read(&self, offset: usize, buf: &mut [u8]) -> Result<(), MemoryError> {
        let memory = self.0.borrow();
        let source = memory
            .get(offset..offset + buf.len())
            .ok_or(MemoryError::OutOfBounds {
                offset,
                len: buf.len(),
                window: memory.len(),
            })?;
        buf.copy_from_slice(source);
        Ok(())
    }
    fn write(&mut self, offset: usize, data: &[u8]) -> Result<(), MemoryError> {
        let mut memory = self.0.borrow_mut();
        let window = memory.len();
        memory
            .get_mut(offset..offset + data.len())
            .ok_or(MemoryError::OutOfBounds {
                offset,
                len: data.len(),
                window,
            })?
            .copy_from_slice(data);
        Ok(())
    }
    fn read_u32(&self, offset: usize) -> Result<u32, MemoryError> {
        let mut word = [0; 4];
        self.read(offset, &mut word)?;
        Ok(u32::from_le_bytes(word))
    }
    fn write_u32(&mut self, offset: usize, value: u32) -> Result<(), MemoryError> {
        self.write(offset, &value.to_le_bytes())
    }
    fn acquire_barrier(&mut self) {}
    fn publish_barrier(&mut self) {}
}

struct SimHal {
    hal: RecordingHal,
    log: Rc<RefCell<Log>>,
}

impl Hal for SimHal {
    fn apply(&mut self, command: HalCommand) -> Result<(), HalError> {
        self.log.borrow_mut().hal.push(command);
        self.hal.apply(command)
    }
    fn read(&mut self, query: Query) -> Result<Reading, HalError> {
        self.hal.read(query)
    }
    fn cached(&self, control: Control) -> Option<Value> {
        self.hal.cached(control)
    }
    fn poll_events(&mut self, out: &mut Vec<HalEvent>) -> Result<(), HalError> {
        self.hal.poll_events(out)
    }
}

/// Splash queue and power backend.
struct Recorder(Rc<RefCell<Log>>);

impl SplashQueue for Recorder {
    type Error = std::io::Error;
    fn send(&mut self, _: SplashCommand) -> std::io::Result<()> {
        Ok(())
    }
}

impl PowerBackend for Recorder {
    type Error = std::io::Error;
    fn perform(&mut self, step: PowerStep) -> std::io::Result<StepStatus> {
        self.0.borrow_mut().power.push(step);
        Ok(StepStatus::Success)
    }
}

struct Settings {
    files: BTreeMap<SettingsFileId, Vec<u8>>,
    log: Rc<RefCell<Log>>,
}

impl SettingsStorage for Settings {
    fn read(&self, file: SettingsFileId) -> std::io::Result<Option<Vec<u8>>> {
        Ok(self.files.get(&file).cloned())
    }
    fn write(&mut self, file: SettingsFileId, bytes: &[u8]) -> std::io::Result<()> {
        self.log.borrow_mut().settings_writes.push(file);
        self.files.insert(file, bytes.to_vec());
        Ok(())
    }
    fn remove(&mut self, file: SettingsFileId) -> std::io::Result<()> {
        self.log.borrow_mut().settings_writes.push(file);
        self.files.remove(&file);
        Ok(())
    }
}

/// Records every request and answers `OK`.
struct Supplicant(Rc<RefCell<Log>>);

impl SupplicantLink for Supplicant {
    fn request(&mut self, command: &str) -> Result<String, LinkError> {
        self.0.borrow_mut().supplicant.push(command.to_owned());
        Ok("OK\n".into())
    }
}

/// Lends every backend, as the Linux platform does, so a service notice can reach the
/// supplicant.
struct Services {
    runner: ScriptedRunner,
    files: MemFiles,
    net: ScriptedNet,
    http: ScriptedHttp,
    supplicant: Supplicant,
}

impl ServiceBackends for Services {
    type Runner = ScriptedRunner;
    type Files = MemFiles;
    type Net = ScriptedNet;
    type Http = ScriptedHttp;
    type Supplicant = Supplicant;
    fn runner(&mut self) -> &mut ScriptedRunner {
        &mut self.runner
    }
    fn files(&mut self) -> &mut MemFiles {
        &mut self.files
    }
    fn net(&mut self) -> &mut ScriptedNet {
        &mut self.net
    }
    fn http(&mut self) -> &mut ScriptedHttp {
        &mut self.http
    }
    fn supplicant(&mut self) -> &mut Supplicant {
        &mut self.supplicant
    }
    fn lend(&mut self) -> Option<ServiceIo<'_, Self>> {
        Some(ServiceIo {
            runner: &mut self.runner,
            files: &mut self.files,
            net: &mut self.net,
            http: &mut self.http,
            supplicant: &mut self.supplicant,
        })
    }
}

/// SHARC memory whose core-0 fault count is 0 for the first two reads and 2 after them.
struct FaultingSharc(Rc<RefCell<Log>>);

impl SharcMemory for FaultingSharc {
    fn write(&mut self, address: u32, bytes: &[u8]) -> std::io::Result<()> {
        self.0
            .borrow_mut()
            .sharc_writes
            .push((address, bytes.len()));
        Ok(())
    }
    fn read(&mut self, address: u32, out: &mut [u8]) -> std::io::Result<()> {
        let mut log = self.0.borrow_mut();
        let count: u32 = if log.sharc_reads.len() < 2 { 0 } else { 2 };
        log.sharc_reads.push((address, out.len()));
        out.copy_from_slice(&count.to_le_bytes()[..out.len()]);
        Ok(())
    }
}

/// A QC without SHARCs: enough for the probe stage and the decorated DSP windows. Every
/// provided `Platform` method answers something other than its default.
struct Sim {
    clock: SimClock,
    hal: SimHal,
    settings: Settings,
    recorder: Recorder,
    services: Services,
    media: PathBuf,
    rings: Mem,
    cores: [Mem; 2],
    /// `poll_input` panics from this time on.
    panic_at: Option<Duration>,
    /// Platform input `poll_input` delivers once its time has come.
    inputs: Vec<(Duration, Input)>,
    log: Rc<RefCell<Log>>,
}

/// Moves the inputs whose time has come to `out`.
fn deliver(inputs: &mut Vec<(Duration, Input)>, now: Duration, out: &mut Vec<Input>) {
    let (due, later): (Vec<_>, Vec<_>) = inputs.drain(..).partition(|(at, _)| *at <= now);
    *inputs = later;
    out.extend(due.into_iter().map(|(_, input)| input));
}

fn no_sharc() -> std::io::Error {
    std::io::Error::other("no SHARC in this simulation")
}

impl Platform for Sim {
    type Error = std::io::Error;
    type Clock = SimClock;
    type BootBus = RecordingBus;
    type Window = Mem;
    type RingDrain = tetrad_dsp_host::drain::MainLoop;
    type Hal = SimHal;
    type Settings = Settings;
    type Splash = Recorder;
    type Power = Recorder;
    type Services = Services;

    fn clock(&self) -> SimClock {
        self.clock.clone()
    }
    fn kernel_cmdline(&mut self) -> std::io::Result<String> {
        Ok("console=ttySC0 productid=quadcortex\n".into())
    }
    fn marker(&mut self, _: Marker) -> std::io::Result<bool> {
        Ok(false)
    }
    fn set_marker(&mut self, _: Marker) -> std::io::Result<()> {
        Ok(())
    }
    fn boot_count(&mut self) -> std::io::Result<u32> {
        self.log.borrow_mut().boot_count_reads += 1;
        Ok(7)
    }
    fn splash(&mut self) -> &mut Recorder {
        &mut self.recorder
    }
    fn power(&mut self) -> &mut Recorder {
        &mut self.recorder
    }
    fn settings(&mut self) -> &mut Settings {
        &mut self.settings
    }
    fn storage(&self) -> StorageRoots {
        StorageRoots {
            user: MediaRoot::new(&self.media),
            factory: MediaRoot::factory(self.media.join("factory")),
            database: self.media.join("zenui.db"),
        }
    }
    fn hal(&mut self) -> &mut SimHal {
        &mut self.hal
    }
    fn services(&mut self) -> &mut Services {
        &mut self.services
    }
    fn firmware(&mut self, _: Variant) -> std::io::Result<SharcFirmware> {
        Err(no_sharc())
    }
    fn boot_bus(&mut self) -> std::io::Result<RecordingBus> {
        Err(no_sharc())
    }
    fn map_soc2_buffer(
        &mut self,
        _: &mut RecordingBus,
        _: PhysAddr,
        _: u32,
    ) -> std::io::Result<()> {
        Err(no_sharc())
    }
    fn open_soc2(&mut self) -> std::io::Result<bool> {
        Err(no_sharc())
    }
    fn dsp_windows(&mut self) -> std::io::Result<DspWindows<Mem>> {
        let window = |core, memory| ModelMemory::new(core, Address::new(MODEL_BASE), memory);
        Ok(DspWindows {
            messages: self.rings.clone(),
            cores: CoreWindows::new(
                window(DspCore::Core0, self.cores[0].clone()).unwrap(),
                window(DspCore::Core1, self.cores[1].clone()).unwrap(),
            )
            .unwrap(),
        })
    }
    fn dsp_file(&mut self, _: DspFile<'_>) -> std::io::Result<Vec<u8>> {
        Err(std::io::ErrorKind::NotFound.into())
    }
    fn poll_input(&mut self, out: &mut Vec<Input>) -> std::io::Result<()> {
        if self.panic_at.is_some_and(|at| self.clock.now() >= at) {
            panic!("injected step panic");
        }
        deliver(&mut self.inputs, self.clock.now(), out);
        Ok(())
    }
    fn midi_write(&mut self, _: &[u8]) -> std::io::Result<()> {
        Ok(())
    }
    fn present(&mut self, _: &[u8]) -> std::io::Result<()> {
        Ok(())
    }
    fn log(&mut self, _: &tetrad_app::device::Notice) {}
    fn random_u32(&mut self) -> Option<u32> {
        Some(RANDOM)
    }
    fn capture_storage(&mut self) -> Option<Box<dyn CaptureStorage>> {
        Some(Box::new(FsStorage))
    }
    fn sharc_memory(&mut self) -> Option<Box<dyn SharcMemory>> {
        Some(Box::new(FaultingSharc(self.log.clone())))
    }
    fn media_cipher(&mut self) -> Option<DeviceCipher> {
        Some(DeviceCipher::seed(
            &FactoryIrSeed::new(&[0x11; SEED_LEN]).unwrap(),
        ))
    }
    fn wall_clock(&mut self) -> Option<SystemTime> {
        Some(wall_clock())
    }
    fn display_panel(&mut self) -> Option<DisplayPanel> {
        Some(DisplayPanel::Fvd)
    }
}

/// Ends the run when the simulated clock reaches `stop_at`, as the stop file would.
struct TestHost {
    clock: SimClock,
    stop_at: Option<Duration>,
}

impl Host for TestHost {
    fn stop_requested(&mut self) -> bool {
        self.stop_at.is_some_and(|at| self.clock.now() >= at)
    }
    fn rss_kib(&mut self) -> Option<u64> {
        None
    }
}

/// A temp `/media/p4` with an empty library and a stock-shaped `zenui.db`, and a log folder.
struct Bench {
    root: PathBuf,
    clock: SimClock,
    log: Rc<RefCell<Log>>,
}

impl Bench {
    fn new(name: &str) -> Self {
        let root =
            std::env::temp_dir().join(format!("tetrad-app-bringup-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("media/Presets")).unwrap();
        fs::create_dir_all(root.join("media/factory")).unwrap();
        rusqlite::Connection::open(root.join("media/zenui.db"))
            .unwrap()
            .execute_batch(DDL)
            .unwrap();
        Self {
            root,
            clock: SimClock::default(),
            log: Rc::default(),
        }
    }

    fn logs(&self) -> PathBuf {
        self.root.join("logs")
    }

    fn options(&self, stage: Stage, limit_ms: u64) -> RunOptions {
        RunOptions::bringup(stage, Duration::from_millis(limit_ms), self.logs())
    }

    fn sim(&self, panic_at: Option<Duration>) -> Sim {
        Sim {
            clock: self.clock.clone(),
            hal: SimHal {
                hal: RecordingHal::default(),
                log: self.log.clone(),
            },
            settings: Settings {
                files: BTreeMap::new(),
                log: self.log.clone(),
            },
            recorder: Recorder(self.log.clone()),
            services: Services {
                runner: ScriptedRunner::default(),
                files: MemFiles::default(),
                net: ScriptedNet::default(),
                http: ScriptedHttp::default(),
                supplicant: Supplicant(self.log.clone()),
            },
            media: self.root.join("media"),
            rings: Mem::new(MESSAGE_MEMORY_BYTES),
            cores: [Mem::new(0x1000), Mem::new(0x1000)],
            panic_at,
            inputs: Vec::new(),
            log: self.log.clone(),
        }
    }

    /// The stop file appears at `stop_at` on the bench clock.
    fn launch(&self, stop_at: Option<Duration>) -> Launch {
        Launch {
            stored_boot_count: 7,
            binary_sha256: None,
            heartbeat: Heartbeat::default(),
            host: Box::new(TestHost {
                clock: self.clock.clone(),
                stop_at,
            }),
        }
    }

    fn decorate<P: Platform>(&self, platform: P, options: &RunOptions) -> Bringup<P> {
        Bringup::new(platform, options, self.launch(None)).unwrap()
    }

    fn start(&self, options: RunOptions, panic_at: Option<Duration>) -> Device<Bringup<Sim>> {
        let platform = self.decorate(self.sim(panic_at), &options);
        Device::start_with(platform, options).unwrap()
    }

    /// One whole bring-up run (`Device::bringup`, as `main`) on the decorated simulation.
    fn run(
        &self,
        options: RunOptions,
        panic_at: Option<Duration>,
        stop_at: Option<Duration>,
    ) -> Exit {
        self.run_on(self.sim(panic_at), options, stop_at)
    }

    fn run_on<P: Platform<RingDrain = tetrad_dsp_host::drain::MainLoop>>(
        &self,
        platform: P,
        options: RunOptions,
        stop_at: Option<Duration>,
    ) -> Exit {
        let platform = Bringup::new(platform, &options, self.launch(stop_at)).unwrap();
        Device::bringup(platform, options).unwrap()
    }

    fn read_log(&self, name: &str) -> String {
        fs::read_to_string(self.logs().join(name)).unwrap()
    }

    /// The headphone mute (word 100, unverified, libzenhal `0x1b158`) reached the HAL, and no
    /// system power step ran.
    fn assert_test_exit(&self) {
        let log = self.log.borrow();
        let mute = Setting::new(
            Control::HeadphoneVolume,
            Value::HeadphoneVolume(HeadphoneVolume::Muted),
        )
        .unwrap();
        let unverified = tetrad_hal::analog::Verification::Unverified;
        assert!(log.hal.contains(&HalCommand::Set(mute, unverified)));
        assert_eq!(
            log.power,
            [],
            "the test exit never runs a system power step"
        );
    }
}

impl Drop for Bench {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

/// Every file under `dir` with its bytes.
fn snapshot(dir: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    let mut files = BTreeMap::new();
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            files.extend(snapshot(&path));
        } else {
            files.insert(path.clone(), fs::read(&path).unwrap());
        }
    }
    files
}

/// The bytes last published on the outgoing ring of `core`.
fn last_sent(memory: &Mem, core: CoreId) -> Vec<u8> {
    let layout = RingLayout::outgoing(core);
    let memory = memory.0.borrow();
    let slot = usize::from(memory[layout.producer_offset]);
    let at = layout.length_offset + slot * 4;
    let len = u32::from_le_bytes(memory[at..at + 4].try_into().unwrap()) as usize;
    let at = layout.payload_offset + slot * layout.slot_bytes;
    memory[at..at + len].to_vec()
}

/// The SHARC side: one response into the incoming ring of `core`.
fn publish(memory: &Mem, core: CoreId, bytes: &[u8]) {
    let layout = RingLayout::incoming(core);
    let mut memory = memory.0.borrow_mut();
    let slot = (memory[layout.producer_offset] + 1) % layout.slot_count;
    let at = layout.payload_offset + usize::from(slot) * layout.slot_bytes;
    memory[at..at + bytes.len()].copy_from_slice(bytes);
    let at = layout.length_offset + usize::from(slot) * 4;
    memory[at..at + 4].copy_from_slice(&(bytes.len() as u32).to_le_bytes());
    memory[layout.producer_offset] = slot;
}

fn mute_frame(value: u32) -> RequestFrame {
    RequestFrame::new(Request::MasterMute {
        core: DspCore::Core0,
        value,
    })
    .unwrap()
}

/// P3/P4: the time limit ends the run through the test exit (headphones 0, no system power
/// step) and the stage verdict lands in the summary.
#[test]
fn time_limit_ends_in_the_test_exit() {
    let bench = Bench::new("time-limit");
    let exit = bench.run(bench.options(Stage::Probe, 1_500), None, None);
    assert_eq!(exit.reason, ExitReason::TimeLimit);
    assert_eq!(exit.verdict, Ok(()));
    assert_eq!(exit.code(), 0);
    assert!(bench.clock.now() >= Duration::from_millis(1_500));
    bench.assert_test_exit();
    assert!(bench.read_log(SUMMARY).contains("STAGE probe PASS"));
}

/// P3: the stop file is polled during the run and ends it long before the time limit.
#[test]
fn stop_file_ends_in_the_test_exit() {
    let bench = Bench::new("stop-file");
    let stop_at = Duration::from_millis(400);
    let exit = bench.run(bench.options(Stage::Probe, 60_000), None, Some(stop_at));
    assert_eq!(exit.reason, ExitReason::StopFile);
    assert!(bench.clock.now() < stop_at + Duration::from_secs(1));
    bench.assert_test_exit();
}

/// P4: a panic inside a step is caught; the test exit still mutes, and the run reports the
/// panic exit code and a failed stage.
#[test]
fn panic_in_a_step_mutes_and_exits_with_the_panic_code() {
    let bench = Bench::new("panic");
    let exit = bench.run(
        bench.options(Stage::Probe, 60_000),
        Some(Duration::from_millis(300)),
        None,
    );
    assert_eq!(exit.reason, ExitReason::Panic("injected step panic".into()));
    assert_eq!(exit.code(), PANIC_EXIT);
    bench.assert_test_exit();
    assert!(
        bench
            .read_log(SUMMARY)
            .contains("STAGE probe FAIL panic: injected step panic")
    );
}

/// P6: without `--persist` nothing under `/media/p4` changes (library, `zenui.db`), the
/// settings backend and the boot counter are never written, and the settings the run would
/// have written land in the log folder for comparison.
#[test]
fn no_persist_writes_nothing_to_real_paths() {
    let bench = Bench::new("no-persist");
    let media = bench.root.join("media");
    let before = snapshot(&media);
    bench.run(bench.options(Stage::Probe, 1_000), None, None);
    assert_eq!(snapshot(&media), before);
    let log = bench.log.borrow();
    assert_eq!(log.settings_writes, []);
    assert_eq!(log.boot_count_reads, 0);
    assert!(
        bench
            .logs()
            .join(WOULD_WRITE)
            .join("ZenjackConfig.ini")
            .is_file()
    );
    assert!(
        bench
            .read_log("run.000.log")
            .contains("WOULD-WRITE ZenjackConfig.ini")
    );
}

/// R17: 48 V is refused where it reaches the hardware. Cortex Control reduces its actions
/// inside the bridge (`control.rs` `act`), as `Event::Action` does here, so the action gate
/// never sees them: TYPE → XLR, then PHANTOM on, must still write no `XlrPhantom`.
#[test]
fn phantom_power_from_a_directly_reduced_action_is_refused() {
    let bench = Bench::new("phantom");
    let mut device = bench.start(bench.options(Stage::Probe, 60_000), None);
    for (id, value) in [(ConnectorId::In1Type, 1.0), (ConnectorId::In1Phantom, 1.0)] {
        device.push(Event::Action(Action::SetConnector { id, value }));
        // Long enough for every jack-scheduler step (the type switch waits 500 ms).
        let until = bench.clock.now() + Duration::from_secs(1);
        while bench.clock.now() < until {
            assert_eq!(device.step(), None);
        }
    }
    let phantom = Value::InputType(InputType::XlrPhantom);
    assert!(!bench.log.borrow().hal.iter().any(
        |command| matches!(command, HalCommand::Set(setting, _) if setting.value() == phantom)
    ));
    assert!(
        bench
            .read_log("run.000.log")
            .contains("bring-up refused 48 V phantom power without --allow-phantom")
    );
    assert_eq!(device.state().io.value(ConnectorId::In1Phantom), 0.0);
    let stored = device
        .state()
        .settings
        .settings()
        .io
        .params
        .get(&ConnectorId::In1Phantom.id())
        .and_then(|param| param.value);
    assert_eq!(stored, Some(0.0));
}

/// P5: sends show a preview except for blob, data and signing-serial types; receives
/// (key vectors `0x24`/`0x25`, signing replies `0x11`/`0x1f`) never show bytes; model-window
/// writes show core, address and length only.
#[test]
fn trace_redacts_key_signing_and_blob_payloads() {
    let bench = Bench::new("trace");
    let options = RunOptions {
        trace: true,
        ..bench.options(Stage::Dsp, 60_000)
    };
    let sim = bench.sim(None);
    let rings = sim.rings.clone();
    let mut platform = bench.decorate(sim, &options);
    let mut windows = platform.dsp_windows().unwrap();
    let mut transport = DspTransport::new(windows.messages).unwrap();

    transport
        .send_control(ControlPacket::Request(&mute_frame(1)))
        .unwrap();
    for (message_type, core, secret) in [
        (MessageType::LoadModel, DspCore::Core2, 0xa5),
        (MessageType::TunerMuteMessage, DspCore::Core3, 0x7e),
    ] {
        let metadata = MessageMetadata {
            message_type: message_type as u8,
            core: CoreId::Dsp(core),
            slot: 1,
            argument: 0,
        };
        let payload = [secret; 64];
        let mut fragments = Fragmenter::new(metadata, &payload, CodecLimits::default()).unwrap();
        transport
            .try_send_fragment(fragments.next().unwrap())
            .unwrap();
    }
    let core0 = CoreId::Dsp(DspCore::Core0);
    for (kind, body) in [
        (ResponseType::Vector24, [0x5c; 16].as_slice()),
        (ResponseType::Vector25, &[0x5c; 16]),
        (ResponseType::Vector27, &[0x5c; 16]),
        (ResponseType::UsbMeterUpdate, &[0x6d; 16]),
        (ResponseType::TunerCalibrate, &[0x6d; 32]),
    ] {
        publish(&rings, core0, &encode(kind, 0, body));
        assert!(transport.try_receive(core0).unwrap().is_some());
    }
    let target = DeploymentTarget {
        core: DspCore::Core0,
        slot: ModuleSlot::try_from(1).unwrap(),
    };
    windows
        .cores
        .get_mut(DspCore::Core0)
        .unwrap()
        .load_model(target, Address::new(MODEL_BASE + 0x100), &[0xb7; 64])
        .unwrap();

    let trace = bench.read_log("trace.000.log");
    for expected in [
        "type=0x20 MasterMute len=",
        "type=0x07 LoadModel len=80 wait=0 bytes=redacted",
        "type=0x35 TunerMuteMessage len=80 wait=0 bytes=redacted",
        "RECV ring=Dsp(Core0) slot=1 type=0x24 Vector24",
        "type=0x25 Vector25",
        "type=0x27 Vector27",
        "type=0x11 UsbMeterUpdate",
        "type=0x1f TunerCalibrate",
        "MODEL core=Core0 address=0x240100 len=64",
    ] {
        assert!(trace.contains(expected), "{expected} missing in:\n{trace}");
    }
    assert!(
        trace.contains(" bytes=20"),
        "mute preview missing in:\n{trace}"
    );
    for secret in ["a5a5", "7e7e", "5c5c", "6d6d", "b7b7"] {
        assert!(!trace.contains(secret), "{secret} leaked into:\n{trace}");
    }
}

/// H1 "no fault count change": from stage `dsp` on, the run reads the core-0 kernel fault
/// count (DM `0x301820`) at the start, in each health line and at the exit. A change fails the
/// stage; the check only ever reads that one word.
#[test]
fn fault_count_change_fails_the_stage_without_a_write() {
    let bench = Bench::new("faults");
    let exit = bench.run(bench.options(Stage::Dsp, 60_000), None, None);

    let run = bench.read_log("run.000.log");
    for expected in [
        "FAULTS start faults core0=0",
        "unmutes_held=0 faults core0=0",
        "unmutes_held=0 faults core0=2",
        "FAULTS exit faults core0=2",
    ] {
        assert!(run.contains(expected), "{expected} missing in:\n{run}");
    }
    let reason = "fault count core0 changed 0 -> 2";
    assert!(exit.verdict.as_ref().unwrap_err().contains(reason));
    assert_eq!(exit.code(), 2);
    assert!(bench.read_log(SUMMARY).contains(reason));
    let log = bench.log.borrow();
    assert_eq!(log.sharc_writes, []);
    assert!(log.sharc_reads.len() >= 4);
    assert!(
        log.sharc_reads
            .iter()
            .all(|&access| access == (FAULT_COUNT_ADDRESS, 4))
    );
}

/// P7: with `--keep-muted` a `0x20` unmute reaches the ring with the mute bit set; a mute
/// passes unchanged.
#[test]
fn keep_muted_holds_every_unmute() {
    let bench = Bench::new("keep-muted");
    let options = RunOptions {
        keep_muted: true,
        ..bench.options(Stage::Deploy, 60_000)
    };
    let sim = bench.sim(None);
    let rings = sim.rings.clone();
    let mut platform = bench.decorate(sim, &options);
    let mut transport = DspTransport::new(platform.dsp_windows().unwrap().messages).unwrap();
    let core0 = CoreId::Dsp(DspCore::Core0);
    for (sent, delivered) in [(0, 1), (3, 3), (2, 3)] {
        transport
            .send_control(ControlPacket::Request(&mute_frame(sent)))
            .unwrap();
        assert_eq!(
            last_sent(&rings, core0),
            mute_frame(delivered).as_bytes(),
            "0x20 value {sent}"
        );
    }
}

/// P3: the guard allows 40 s per start-up phase and 10 s per loop pass after it, and ends
/// any run 30 s past its limit.
#[test]
fn guard_aborts_a_stalled_loop() {
    let s = Duration::from_secs;
    let limit = s(120);
    let heartbeat = Heartbeat::default();
    heartbeat.beat(s(5));
    assert_eq!(heartbeat.verdict(s(44), limit), None);
    assert!(matches!(
        heartbeat.verdict(s(46), limit),
        Some(Stall::Stalled { running: false, .. })
    ));
    heartbeat.set_running();
    heartbeat.beat(s(60));
    assert_eq!(heartbeat.verdict(s(69), limit), None);
    assert!(matches!(
        heartbeat.verdict(s(71), limit),
        Some(Stall::Stalled { running: true, .. })
    ));
    heartbeat.beat(s(151));
    assert_eq!(heartbeat.verdict(s(151), limit), Some(Stall::Overrun));
}

/// `tetrad bringup --owned` parses the model names and refuses the map without
/// `--allow-owned` before the platform opens; an unknown name is a parse error. Only
/// rejected options run here: nothing reaches the hardware.
#[cfg(target_os = "linux")]
#[test]
fn bringup_cli_refuses_owned_blocks_without_allow_owned() {
    let run = |model: &str| {
        let output = std::process::Command::new(env!("CARGO_BIN_EXE_tetrad"))
            .args([
                "bringup",
                "--stage",
                "deploy",
                "--preset",
                "x.pb",
                "--time-limit",
                "60",
            ])
            .args(["--log-dir", "/nonexistent/tetrad-test", "--owned", "volume"])
            .args(["--owned", model])
            .output()
            .unwrap();
        assert!(!output.status.success());
        String::from_utf8(output.stderr).unwrap()
    };
    assert!(run("gain").contains("--owned needs --allow-owned in a bring-up run"));
    assert!(run("fuzz").contains("unknown owned block \"fuzz\""));
}

/// review-w4-dsp F2: owned blocks run only in a bring-up run until proof H1 passes, so `run`
/// has no `--owned` flag. clap rejects it before `main` runs: nothing reaches the hardware.
#[cfg(target_os = "linux")]
#[test]
fn run_cli_rejects_owned_blocks() {
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_tetrad"))
        .args(["run", "--owned", "volume"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("unexpected argument '--owned'"), "{stderr}");
}

/// Review B1: the decorator forwards every provided `Platform` method (a default would hide
/// the inner backend). With the FVD panel the stored 50 % brightness gives level 5 (50 / 9),
/// as stock, not 50.
#[test]
fn bringup_forwards_every_provided_platform_method() {
    let bench = Bench::new("forward");
    let persist = RunOptions {
        persist: true,
        ..bench.options(Stage::Full, 60_000)
    };
    let mut platform = bench.decorate(bench.sim(None), &persist);
    assert_eq!(platform.random_u32(), Some(RANDOM));
    assert!(platform.capture_storage().is_some());
    assert!(platform.sharc_memory().is_some());
    assert!(platform.media_cipher().is_some());
    assert_eq!(platform.wall_clock(), Some(wall_clock()));
    assert_eq!(platform.display_panel(), Some(DisplayPanel::Fvd));
    drop(platform);

    let _device = bench.start(bench.options(Stage::Probe, 60_000), None);
    let level = Brightness::try_from(5).unwrap();
    let backlight = HalCommand::Display(DisplayCommand::Brightness(level));
    assert!(bench.log.borrow().hal.contains(&backlight));
}

/// Review B2: without `--services` a supplicant event never reaches the Wi-Fi state machine,
/// so no supplicant request (`DISABLE_NETWORK`, `SAVE_CONFIG`) is sent; the run log names the
/// drop by kind only.
#[test]
fn supplicant_events_change_nothing_without_services() {
    let bench = Bench::new("no-services");
    let mut sim = bench.sim(None);
    let connected =
        "<3>CTRL-EVENT-CONNECTED - Connection to 02:00:00:00:00:01 completed [id=1 id_str=]";
    sim.inputs.push((
        Duration::ZERO,
        Input::Service(ServiceNotice::Supplicant(connected.into())),
    ));
    bench.run_on(sim, bench.options(Stage::Probe, 1_000), None);
    assert_eq!(bench.log.borrow().supplicant, Vec::<String>::new());
    let run = bench.read_log("run.000.log");
    assert!(run.contains("DROPPED supplicant event without --services"));
    assert!(!run.contains("02:00:00:00:00:01"));
}

/// Review B8: the decorator refuses a HAL write that selects 48 V phantom power even when it
/// bypasses `Device::execute`.
#[test]
fn phantom_hal_write_is_refused_at_the_last_line() {
    let bench = Bench::new("phantom-hal");
    let mut platform = bench.decorate(bench.sim(None), &bench.options(Stage::Probe, 60_000));
    let phantom = Value::InputType(InputType::XlrPhantom);
    let setting = Setting::new(Control::InputType(Jack::One), phantom).unwrap();
    let command = HalCommand::Set(setting, Verification::Unverified);
    assert!(platform.hal().apply(command).is_err());
    assert!(!bench.log.borrow().hal.contains(&command));
    assert!(
        bench
            .read_log("run.000.log")
            .contains("48 V phantom power without --allow-phantom")
    );
}

/// Review bring-up safety 2 R7: every Zenjack power command is refused at the decorator, as
/// no tetrad-app path emits one (the stock teardown goes through `PowerBackend`).
#[test]
fn power_hal_commands_are_refused_at_the_last_line() {
    use tetrad_hal::zenjack::{PowerButtonSensitivity, PowerCommand};
    let bench = Bench::new("power-hal");
    let mut options = bench.options(Stage::Full, 60_000);
    options.power = PowerMode::Stock;
    let mut platform = bench.decorate(bench.sim(None), &options);
    let commands = [
        PowerCommand::Restart,
        PowerCommand::Shutdown,
        PowerCommand::AutoOn(true),
        PowerCommand::ButtonSensitivity(PowerButtonSensitivity::try_from(2).unwrap()),
    ];
    for command in commands {
        assert!(matches!(
            platform.hal().apply(HalCommand::Power(command)),
            Err(HalError::Refused(_))
        ));
    }
    assert!(bench.log.borrow().hal.is_empty());
    let run = bench.read_log("run.000.log");
    assert_eq!(
        run.matches("REFUSED HAL Power(").count(),
        commands.len(),
        "{run}"
    );
}

/// Review bring-up safety 2 R10: stage dsp runs only with `--keep-muted` (a standby wake or a
/// capture entry can send `0x20` value 0).
#[test]
fn stage_dsp_needs_keep_muted() {
    let bench = Bench::new("dsp-muted");
    let mut options = bench.options(Stage::Dsp, 60_000);
    let error = options.validate().unwrap_err().to_string();
    assert!(error.contains("stage dsp needs --keep-muted"), "{error}");
    options.keep_muted = true;
    assert!(options.validate().is_ok());
}

/// Review B9: the stop file also ends start-up, at the next splash phase. The start-up exit
/// mutes the headphones without a system power step, and the summary gets a failed stage.
#[test]
fn stop_file_during_start_up_ends_in_the_start_up_exit() {
    let bench = Bench::new("startup-stop");
    let exit = bench.run(
        bench.options(Stage::Probe, 60_000),
        None,
        Some(Duration::ZERO),
    );
    assert_eq!(exit.reason, ExitReason::StopFile);
    assert_eq!(exit.code(), 2);
    bench.assert_test_exit();
    let run = bench.read_log("run.000.log");
    for expected in [
        "EXIT stop file /tmp/tetrad.stop during start-up",
        "EXIT start-up headphones muted",
    ] {
        assert!(run.contains(expected), "{expected} missing in:\n{run}");
    }
    assert!(
        bench
            .read_log(SUMMARY)
            .contains("STAGE probe FAIL start-up ended by stop file")
    );
}

/// Review B10: one `MODEL-START` run-log line per model start (core, slot, started) from
/// single and bulk replies, and the diagnostics count per core (sent/answered); no reply
/// bytes reach the log.
#[test]
fn model_starts_and_diagnostics_replies_reach_the_run_log() {
    let bench = Bench::new("model-start");
    let sim = bench.sim(None);
    let rings = sim.rings.clone();
    let mut platform = bench.decorate(sim, &bench.options(Stage::Deploy, 60_000));
    let mut transport = DspTransport::new(platform.dsp_windows().unwrap().messages).unwrap();
    let core0 = CoreId::Dsp(DspCore::Core0);
    let request = RequestFrame::new(Request::DiagnosticsRequest { core: core0 }).unwrap();
    transport
        .send_control(ControlPacket::Request(&request))
        .unwrap();
    let words =
        |words: [u32; 3]| -> Vec<u8> { words.iter().flat_map(|w| w.to_le_bytes()).collect() };
    for frame in [
        encode(ResponseType::Diagnostics, 0, &[0x6d; 64]),
        encode(ResponseType::ModelStart, 0, &words([0, 3, 1])),
        encode(ResponseType::ModelStart, 0, &words([0, 4, 0])),
        encode(ResponseType::BulkModelStart, 0, &words([1, 1 << 5, 1 << 6])),
    ] {
        publish(&rings, core0, &frame);
        assert!(transport.try_receive(core0).unwrap().is_some());
    }
    assert_eq!(
        platform.rings.take_diagnostics(),
        "diag core0=1/1 core1=0/0 core2=0/0 core3=0/0 control=0/0"
    );
    let run = bench.read_log("run.000.log");
    for expected in [
        "MODEL-START ring=core0 core=0 slot=3 started=true",
        "MODEL-START ring=core0 core=0 slot=4 started=false",
        "MODEL-START ring=core0 core=1 slot=5 started=true",
        "MODEL-START ring=core0 core=1 slot=6 started=false",
    ] {
        assert!(run.contains(expected), "{expected} missing in:\n{run}");
    }
    assert!(!run.contains("6d6d"));
}

/// Review B16: `--power stock` runs the stock teardown without `killall tee` or `killall
/// tail` (plan §1.6), and `--persist` keeps the stored boot count (stock counted this boot).
#[test]
fn stock_power_skips_killall_and_keeps_the_boot_count() {
    let bench = Bench::new("power-stock");
    let mut sim = bench.sim(None);
    sim.inputs.push((
        Duration::from_millis(500),
        Input::Power(PowerAction::PowerOff),
    ));
    let options = RunOptions {
        persist: true,
        power: PowerMode::Stock,
        ..bench.options(Stage::Full, 60_000)
    };
    let exit = bench.run_on(sim, options, None);
    assert_eq!(exit.reason, ExitReason::Power(PowerAction::PowerOff));
    let log = bench.log.borrow();
    assert!(log.power.contains(&PowerStep::Sync), "{:?}", log.power);
    for killall in [PowerStep::StopLogPipelines, PowerStep::StopLogFollower] {
        assert!(!log.power.contains(&killall), "{killall:?} ran");
    }
    assert_eq!(log.boot_count_reads, 0);
    assert!(
        bench
            .read_log("run.000.log")
            .contains("REFUSED power step StopLogPipelines")
    );
}

/// `tetrad bringup --check` validates before anything opens (review B11; `tetrad-stage.sh` runs
/// it before it stops stock ZenUI). Only a rejected option set runs here.
#[cfg(target_os = "linux")]
#[test]
fn bringup_check_validates_before_the_platform_opens() {
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_tetrad"))
        .args(["bringup", "--check", "--stage", "dsp", "--time-limit", "60"])
        .args(["--log-dir", "/nonexistent/tetrad-test", "--mute-probe"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.contains("--mute-probe needs stage deploy or full"),
        "{stderr}"
    );
}

/// `tetrad <run|bringup> --check` under an SSH-like caller umask 077 (S5 wrote 0600/0700):
/// Tetrad sets init's 022 itself (A2), so its files are 0644 and its folders 0755.
#[test]
fn run_and_bringup_set_the_init_umask() {
    let bin = env!("CARGO_BIN_EXE_tetrad");
    let check = |args: &str| {
        let output = std::process::Command::new("sh")
            .args(["-c", &format!("umask 077; exec \"$0\" {args}"), bin])
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&output.stdout).into_owned()
            + &String::from_utf8_lossy(&output.stderr);
        assert!(text.contains("umask 0022 (caller 0077)"), "{args}: {text}");
        output.status.code()
    };
    let bringup = "bringup --check --stage probe --time-limit 10 --log-dir /nonexistent/zr";
    assert_eq!(check(bringup), Some(0));
    // No host has a supported CorOS ZenUI at /opt/neuraldsp: the firmware exit.
    let firmware = tetrad_app::device::linux::WRONG_FIRMWARE_EXIT;
    assert_eq!(check("run --check"), Some(i32::from(firmware)));
}

/// The scripted QC of `tests/common` (SOC1/SOC2 boot, keys, catalog, deploys) with scripted
/// platform input and a core-0 fault count that reads 1 once model code reached a core
/// window: a fault during the first deploy.
struct Scripted {
    sim: common::Sim,
    inputs: Vec<(Duration, Input)>,
}

/// The fault count of [`Scripted`]. Never written.
struct DeployFaults([common::Window; 2]);

impl SharcMemory for DeployFaults {
    fn write(&mut self, _: u32, _: &[u8]) -> std::io::Result<()> {
        Err(std::io::Error::other("the fault check never writes"))
    }
    fn read(&mut self, _: u32, out: &mut [u8]) -> std::io::Result<()> {
        let loaded = self
            .0
            .iter()
            .any(|window| window.0.borrow().iter().any(|&byte| byte != 0));
        out.copy_from_slice(&u32::from(loaded).to_le_bytes()[..out.len()]);
        Ok(())
    }
}

impl Platform for Scripted {
    type Error = std::io::Error;
    type Clock = common::SimClock;
    type BootBus = common::Bus;
    type Window = common::Window;
    type RingDrain = tetrad_dsp_host::drain::MainLoop;
    type Hal = common::SimHal;
    type Settings = common::Settings;
    type Splash = common::Recorder;
    type Power = common::Recorder;
    type Services = common::Services;

    fn clock(&self) -> common::SimClock {
        self.sim.clock()
    }
    fn kernel_cmdline(&mut self) -> std::io::Result<String> {
        self.sim.kernel_cmdline()
    }
    fn marker(&mut self, marker: Marker) -> std::io::Result<bool> {
        self.sim.marker(marker)
    }
    fn set_marker(&mut self, marker: Marker) -> std::io::Result<()> {
        self.sim.set_marker(marker)
    }
    fn boot_count(&mut self) -> std::io::Result<u32> {
        self.sim.boot_count()
    }
    fn splash(&mut self) -> &mut common::Recorder {
        self.sim.splash()
    }
    fn power(&mut self) -> &mut common::Recorder {
        Platform::power(&mut self.sim)
    }
    fn settings(&mut self) -> &mut common::Settings {
        self.sim.settings()
    }
    fn storage(&self) -> StorageRoots {
        self.sim.storage()
    }
    fn hal(&mut self) -> &mut common::SimHal {
        self.sim.hal()
    }
    fn services(&mut self) -> &mut common::Services {
        self.sim.services()
    }
    fn firmware(&mut self, variant: Variant) -> std::io::Result<SharcFirmware> {
        self.sim.firmware(variant)
    }
    fn boot_bus(&mut self) -> std::io::Result<common::Bus> {
        self.sim.boot_bus()
    }
    fn map_soc2_buffer(
        &mut self,
        bus: &mut common::Bus,
        buffer: PhysAddr,
        bytes: u32,
    ) -> std::io::Result<()> {
        self.sim.map_soc2_buffer(bus, buffer, bytes)
    }
    fn open_soc2(&mut self) -> std::io::Result<bool> {
        self.sim.open_soc2()
    }
    fn dsp_windows(&mut self) -> std::io::Result<DspWindows<common::Window>> {
        self.sim.dsp_windows()
    }
    fn dsp_file(&mut self, file: DspFile<'_>) -> std::io::Result<Vec<u8>> {
        self.sim.dsp_file(file)
    }
    fn poll_input(&mut self, out: &mut Vec<Input>) -> std::io::Result<()> {
        deliver(&mut self.inputs, self.sim.clock.now(), out);
        self.sim.poll_input(out)
    }
    fn midi_write(&mut self, message: &[u8]) -> std::io::Result<()> {
        self.sim.midi_write(message)
    }
    fn present(&mut self, rgb: &[u8]) -> std::io::Result<()> {
        self.sim.present(rgb)
    }
    fn log(&mut self, notice: &tetrad_app::device::Notice) {
        self.sim.log(notice);
    }
    fn sharc_memory(&mut self) -> Option<Box<dyn SharcMemory>> {
        Some(Box::new(DeployFaults(self.sim.core_windows.clone())))
    }
}

/// The device path of the one preset of [`scripted`].
const LIGHT: &str = "/media/p4/Presets/My Presets/Light.pb";

/// The scripted QC over a library with one preset ([`LIGHT`]), its request log and media.
fn scripted(inputs: Vec<(Duration, Input)>) -> (Scripted, Rc<RefCell<common::Log>>, common::Media) {
    let preset = common::light_preset(&common::STOCK.specs(), 0);
    let media = common::Media::new([("Light", preset)]);
    let log = Rc::new(RefCell::new(common::Log::default()));
    let sim = common::Sim::new(common::STOCK, &media, &log);
    (Scripted { sim, inputs }, log, media)
}

/// Review B5: the fault-count baseline is read after the DSP boot and before the first
/// deploy (`FAULTS boot`), so a fault during the blob load (H1: the owned load) fails the
/// stage. Before the fix the first read came after the deploy and the stage passed.
#[test]
fn a_fault_during_the_first_deploy_fails_the_stage() {
    let bench = Bench::new("deploy-fault");
    let (platform, _log, _media) = scripted(Vec::new());
    let options = RunOptions {
        preset: Some(LIGHT.into()),
        ..bench.options(Stage::Deploy, 20_000)
    };
    let exit = bench.run_on(platform, options, None);
    let run = bench.read_log("run.000.log");
    for expected in ["FAULTS boot faults core0=0", "FAULTS start faults core0=1"] {
        assert!(run.contains(expected), "{expected} missing in:\n{run}");
    }
    let reason = "fault count core0 changed 0 -> 1";
    assert!(exit.verdict.unwrap_err().contains(reason));
}

/// Review B6: `summary.log` holds the S2 evidence, so a failed stage needs no second
/// session: markers and start kind, boot writes per region, the SOC2 buffer and gate, boot
/// key reply lengths (never bytes), the catalog model count, stock tables and IR library;
/// the run log has each core-control action.
#[test]
fn dsp_stage_summary_holds_the_boot_evidence() {
    let bench = Bench::new("boot-evidence");
    let (platform, _log, _media) = scripted(Vec::new());
    bench.run_on(platform, bench.options(Stage::Dsp, 12_000), None);
    let summary = bench.read_log(SUMMARY);
    let reply = |kind: ResponseType, key: &[u8]| {
        format!("{:#04x}={}B", kind as u8, encode(kind, 0, key).len())
    };
    for expected in [
        "SharcsStarted=false".to_owned(),
        "boot start=First".into(),
        "other=0/0B".into(),
        format!("soc2 buffer {:#010x} len 0x48: mapped", common::SOC2_BUFFER),
        "soc2 gate high".into(),
        reply(ResponseType::Vector24, &common::REPO_KEY),
        reply(ResponseType::Vector25, &common::BLOB_KEY),
        format!(
            "models {} tables=absent ir_library=absent",
            common::STOCK.specs().models.len()
        ),
    ] {
        assert!(
            summary.contains(&expected),
            "{expected} missing in:\n{summary}"
        );
    }
    assert!(bench.read_log("run.000.log").contains("BOOT control Core1"));
}

/// Review B7: `--power stock` holds the teardown's `0x20` value 0 (an unmute under the
/// bring-up reading of bit 0, inferred until S4a): every `0x20` after the power action
/// reaches the DSP with bit 0 set.
#[test]
fn stock_power_holds_the_value_0_mute() {
    let bench = Bench::new("power-hold");
    let off_at = Duration::from_secs(15);
    let (platform, log, _media) = scripted(vec![(off_at, Input::Power(PowerAction::PowerOff))]);
    let options = RunOptions {
        preset: Some(LIGHT.into()),
        power: PowerMode::Stock,
        ..bench.options(Stage::Deploy, 60_000)
    };
    let exit = bench.run_on(platform, options, None);
    assert_eq!(exit.reason, ExitReason::Power(PowerAction::PowerOff));
    let mutes: Vec<u32> = log
        .borrow()
        .requests
        .iter()
        .filter(|sent| sent.at >= off_at && sent.kind() == MessageType::MasterMute as u8)
        .map(
            |sent| match RequestFrame::decode(&sent.frame).unwrap().request() {
                Request::MasterMute { value, .. } => value,
                other => panic!("{other:?}"),
            },
        )
        .collect();
    assert_eq!(mutes, [1, 1]);
}

/// The Boot Selector switch writes the launcher's `no-chooser` file, which changes the next
/// boot, so a bring-up run refuses it without `--persist`.
#[test]
fn the_boot_selector_switch_needs_persist() {
    use tetrad_app::device::ui::Handoff;
    let bench = Bench::new("boot-selector");
    let full = bench.options(Stage::Full, 60_000);
    let persist = RunOptions {
        persist: true,
        ..full.clone()
    };
    let handoff = Handoff::BootSelector { shown: false };
    assert_eq!(
        full.handoff_refusal(&handoff),
        Some("a launcher file write without --persist")
    );
    assert_eq!(persist.handoff_refusal(&handoff), None);
    assert_eq!(RunOptions::STOCK.handoff_refusal(&handoff), None);
}

/// Review bring-up safety 2 R3: Reset Wi-Fi (`REMOVE_NETWORK all` + `SAVE_CONFIG`) loses SSH,
/// so every bring-up run refuses it, also with `--services --persist`; a Wi-Fi connect or
/// forget writes `wpa_supplicant.conf`, so it needs `--persist`. A stock run refuses none.
#[test]
fn wifi_reset_is_refused_in_every_bringup_run() {
    use tetrad_app::device::ui::Handoff;
    use tetrad_ui::{DeviceAction, UiCommand};
    let bench = Bench::new("wifi");
    let services = RunOptions {
        services: true,
        ..bench.options(Stage::Full, 60_000)
    };
    let persist = RunOptions {
        persist: true,
        ..services.clone()
    };
    let reset = Handoff::Services(UiCommand::Device {
        action: DeviceAction::ResetWifi,
    });
    for options in [&services, &persist] {
        assert!(options.handoff_refusal(&reset).is_some());
    }
    for handoff in [
        UiCommand::WifiConnect {
            ssid: "Home".into(),
            password: None,
        },
        UiCommand::WifiForget {
            ssid: "Home".into(),
        },
    ]
    .map(Handoff::Services)
    {
        assert_eq!(
            services.handoff_refusal(&handoff),
            Some("a wpa_supplicant.conf write without --persist")
        );
        assert_eq!(persist.handoff_refusal(&handoff), None);
        assert_eq!(RunOptions::STOCK.handoff_refusal(&handoff), None);
    }
    assert_eq!(RunOptions::STOCK.handoff_refusal(&reset), None);
}

/// Review r30 C4: with `--persist` but without `--services`, the Cortex Control cloud
/// downloads (CloudProduct 46 DOWNLOAD, ProcessDownloadsQueue 45) are refused as effects, not
/// later as a lost cloud job; with both flags they run.
#[test]
fn control_cloud_downloads_need_services() {
    use tetrad_app::device::Effect;
    use tetrad_app::device::control::AccountRequest;
    use tetrad_services::cloud::services::ProductKind;
    let bench = Bench::new("control-cloud");
    let persist = RunOptions {
        persist: true,
        ..bench.options(Stage::Full, 60_000)
    };
    let both = RunOptions {
        services: true,
        ..persist.clone()
    };
    let effects = [
        AccountRequest::CloudProductDownload {
            kind: ProductKind::NeuralCapture,
            id: "cloud-1".into(),
        },
        AccountRequest::DownloadsQueue(ProductKind::Preset),
    ]
    .map(Effect::ControlAccount);
    for effect in &effects {
        assert_eq!(
            persist.refusal(effect),
            Some("a cloud download without --services")
        );
        assert_eq!(both.refusal(effect), None);
    }
}

/// Bring-up safety 3, S3-1: the NC2 Abort (Cortex Control 63 state 6) and RESTART send
/// `Effect::Cloud(Nc2Delete)` (`DELETE user/products/<id>`). It needs `--persist` as well as
/// `--services`; the NC2 job and product GETs need `--services` only.
#[test]
fn an_nc2_product_delete_needs_persist() {
    use tetrad_app::device::Effect;
    use tetrad_app::device::services::CloudRequest;
    let bench = Bench::new("nc2-delete");
    let none = bench.options(Stage::Full, 60_000);
    let services = RunOptions {
        services: true,
        ..none.clone()
    };
    let persist = RunOptions {
        persist: true,
        ..services.clone()
    };
    let delete = Effect::Cloud(CloudRequest::Nc2Delete("products/nc-1".into()));
    assert_eq!(
        services.refusal(&delete),
        Some("a cloud product delete without --persist")
    );
    assert_eq!(persist.refusal(&delete), None);
    assert_eq!(RunOptions::STOCK.refusal(&delete), None);
    for read in [
        CloudRequest::Nc2Job("jobs/1".into()),
        CloudRequest::Nc2Download("nc-1".into()),
    ]
    .map(Effect::Cloud)
    {
        assert_eq!(services.refusal(&read), None);
        assert_eq!(
            none.refusal(&read),
            Some("services in a bring-up run without --services")
        );
    }
}

/// Review w6-device F5: with `--services` but without `--persist`, bring-up refuses every
/// cloud account write: from the UI the backup delete and rename (r29-media), sign-in (pair),
/// sign-out (unpair), a Directory upload and its overwrite answer, and the sidecar `cloudId`
/// write of a good upload (r31-upload);
/// from Cortex Control the licence refresh (an activation can use up one of the account's)
/// and the sign-out. `--persist` lets them run; `tetrad run` refuses none.
#[test]
fn cloud_account_writes_need_persist() {
    let bench = Bench::new("cloud");
    // Review-r45 M8 (re-r44 §3, O): `--control-cloud` changes the route only, never a refusal.
    for control_cloud in [false, true] {
        let services = RunOptions {
            services: true,
            control_cloud,
            ..bench.options(Stage::Full, 60_000)
        };
        services.validate().unwrap();
        cloud_account_writes_refused(&services);
    }
}

/// [`cloud_account_writes_need_persist`] with `--services` (`services`), with and without
/// `--persist`.
fn cloud_account_writes_refused(services: &RunOptions) {
    use tetrad_app::device::Effect;
    use tetrad_app::device::control::AccountRequest;
    use tetrad_app::device::ui::Handoff;
    use tetrad_ui::{DeviceAction, UiCommand};
    let persist = RunOptions {
        persist: true,
        ..services.clone()
    };
    let handoffs = [
        UiCommand::DeleteBackup { id: 1 },
        UiCommand::UpdateBackup { id: 1 },
        UiCommand::RenameBackup {
            id: 1,
            name: "Gig".into(),
        },
        UiCommand::CloudLogin {
            email: "a@b.c".into(),
            password: "pw".into(),
        },
        UiCommand::Device {
            action: DeviceAction::CloudLogout,
        },
        UiCommand::Upload { items: Vec::new() },
        UiCommand::UploadOverwrite { overwrite: true },
    ]
    .map(Handoff::Services);
    let effects =
        [AccountRequest::RefreshLicences, AccountRequest::SignOut].map(Effect::ControlAccount);
    for handoff in &handoffs {
        assert_eq!(
            services.handoff_refusal(handoff),
            Some("a cloud account write in a bring-up run without --persist")
        );
        assert_eq!(persist.handoff_refusal(handoff), None);
        assert_eq!(RunOptions::STOCK.handoff_refusal(handoff), None);
    }
    for effect in &effects {
        assert_eq!(
            services.refusal(effect),
            Some("a cloud account write without --persist")
        );
        assert_eq!(persist.refusal(effect), None);
        assert_eq!(RunOptions::STOCK.refusal(effect), None);
    }
    // r31-upload: the sidecar `cloudId` write after a good upload reply.
    let written = Effect::PresetUploaded {
        path: "/media/p4/Presets/My Presets/A.pb".into(),
        name: "A".into(),
        cloud_id: "cloud-1".into(),
    };
    assert_eq!(
        services.refusal(&written),
        Some("a preset sidecar write without --persist")
    );
    assert_eq!(persist.refusal(&written), None);
    // r36-upload: the capture `cloudUUID` write after a good capture upload reply.
    let written = Effect::CaptureUploaded {
        hash: "h".into(),
        name: "A".into(),
        cloud_id: "cloud-1".into(),
    };
    assert_eq!(
        services.refusal(&written),
        Some("a capture cloudUUID write without --persist")
    );
    assert_eq!(persist.refusal(&written), None);
    // Other services work (a Wi-Fi scan) still runs with `--services` alone.
    assert_eq!(
        services.handoff_refusal(&Handoff::Services(UiCommand::WifiScan)),
        None
    );
    // r27-media: the NC editor DOWNLOAD stores into Downloads, so it waits for `--persist`
    // before its search goes out.
    // r28-ui MR 2: a Directory cloud row DOWNLOAD stores the same way, so it is refused
    // before the download, not only at the store step.
    for download in [
        Handoff::Services(UiCommand::DownloadCapture { hash: "h".into() }),
        Handoff::Services(UiCommand::DownloadProduct { id: "id".into() }),
        Handoff::Services(UiCommand::DownloadProducts {
            ids: vec!["a".into(), "b".into()],
        }),
    ] {
        assert_eq!(
            services.handoff_refusal(&download),
            Some("a Downloads store write without --persist")
        );
        assert_eq!(persist.handoff_refusal(&download), None);
    }
}

/// Review-r21 H4: bring-up refuses the update commands before their pre-steps (state
/// Updating, Control lock, `MasterMute(3)`, input mask), also with `--services --persist`:
/// a refused install job alone would leave the device muted and locked. A plain check and a
/// cancel still run; `tetrad run` ([`RunOptions::STOCK`]) refuses none of them.
#[test]
fn update_commands_are_refused_before_their_pre_steps() {
    use tetrad_app::device::Effect;
    use tetrad_app::device::control::AccountRequest;
    use tetrad_app::device::ui::Handoff;
    use tetrad_ui::{UiCommand, UpdaterCommand};
    let bench = Bench::new("update");
    let options = RunOptions {
        services: true,
        persist: true,
        ..bench.options(Stage::Full, 60_000)
    };
    let refused = Some("an update download or install");
    for command in [UpdaterCommand::Install, UpdaterCommand::CheckAndDownload] {
        let handoff = Handoff::Services(UiCommand::Updater { command });
        assert_eq!(options.handoff_refusal(&handoff), refused);
        assert_eq!(RunOptions::STOCK.handoff_refusal(&handoff), None);
        let effect = Effect::ControlAccount(AccountRequest::Updater(command));
        assert_eq!(options.refusal(&effect), refused);
        assert_eq!(RunOptions::STOCK.refusal(&effect), None);
    }
    let restart = Effect::ControlAccount(AccountRequest::RestartDownloadViaControl);
    assert_eq!(options.refusal(&restart), refused);
    for command in [UpdaterCommand::Check, UpdaterCommand::CancelDownload] {
        let effect = Effect::ControlAccount(AccountRequest::Updater(command));
        assert_eq!(options.refusal(&effect), None);
        let handoff = Handoff::Services(UiCommand::Updater { command });
        assert_eq!(options.handoff_refusal(&handoff), None);
    }
}

/// INSTALL under `run` takes the stock pre-steps and then needs the Tetrad installer: without
/// `tetrad-update.sh` the job refuses (no fall-back to the stock script, which would drop the boot
/// hook), and the inputs and outputs come back.
#[test]
fn a_control_install_under_run_needs_the_tetrad_installer() {
    use tetrad_control::codecs::{MessageAction, UpdaterCommand, UpdaterMessage};
    let (mut platform, log, _media) = scripted(Vec::new());
    let sim = &mut platform.sim;
    (sim.services().files()).put(tetrad_media::factory::SERIAL_PATH, "TEST12345\n");
    let account = b"[General]\nusername=fake-user\n";
    sim.settings()
        .write(SettingsFileId::Account, account)
        .unwrap();
    let mut device = Device::start_with(platform, RunOptions::STOCK).unwrap();
    common::connect_control(&mut device);
    device.push(Event::Service(ServiceNotice::Download(Ok(()))));
    (0..20).for_each(|_| _ = device.step());
    let requests = log.borrow().requests.len();
    let install = UpdaterMessage {
        action: Some(MessageAction::Update.into()),
        command: Some(UpdaterCommand::Install.into()),
        ..Default::default()
    };
    common::host_send(&mut device, install);
    (0..5).for_each(|_| _ = device.step());
    let notices = &device.state().notices;
    assert!(
        notices.iter().any(|notice| notice.detail.contains(
            "update install failed: the Tetrad installer /media/p4/tetrad/launcher/tetrad-update.sh is missing"
        )),
        "{notices:?}"
    );
    assert!(
        log.borrow()
            .kinds(requests)
            .contains(&(MessageType::MasterMute as u8))
    );
    assert!(!device.bridges().services.inputs_masked());
}

/// A14 (s4-usb-in §5.1): a SOC2 ARM block reaches the run log as one `USB` line of the stock
/// USB Statistics rows, once per new block; a second line needs a new block.
#[test]
fn soc2_usb_statistics_reach_the_run_log() {
    use tetrad_dsp_host::responses::Event as DspEvent;
    use tetrad_dsp_protocol::responses::Soc2ArmDiagnostics;
    let bench = Bench::new("usb-stats");
    let mut device = bench.start(bench.options(Stage::Probe, 3_000), None);
    let mut record = [0_u8; 128];
    record[0] = 1;
    record[0x18..0x20].copy_from_slice(&48_000_u64.to_le_bytes());
    record[0x38..0x3c].copy_from_slice(&2_u32.to_le_bytes());
    let block = DspEvent::Soc2ArmDiagnostics(Soc2ArmDiagnostics(record));
    device.push(Event::Dsp(CoreId::Control, block));
    let exit = device.run_bringup();
    assert_eq!(exit.reason, ExitReason::TimeLimit);
    let run = bench.read_log("run.000.log");
    let lines: Vec<_> = run.lines().filter(|line| line.contains(" USB ")).collect();
    assert_eq!(lines.len(), 1, "{run}");
    for row in [
        "USBConnected=Yes",
        "USBAudioRxFrame=48000",
        "USBAudioOUTGapsDetected=2",
    ] {
        assert!(lines[0].contains(row), "{row} missing in {}", lines[0]);
    }
}

/// The scripted QC in a bring-up run with `options`, its request log and the SHARC rings. The
/// account is signed in and the serial is set, so Control can run its key exchange.
fn scripted_bringup(
    bench: &Bench,
    options: RunOptions,
) -> (
    Device<Bringup<Scripted>>,
    Rc<RefCell<common::Log>>,
    common::Window,
    common::Media,
) {
    let (mut platform, log, media) = scripted(Vec::new());
    let sim = &mut platform.sim;
    (sim.services().files()).put(tetrad_media::factory::SERIAL_PATH, "TEST12345\n");
    let account = b"[General]\nusername=fake-user\n";
    sim.settings()
        .write(SettingsFileId::Account, account)
        .unwrap();
    let rings = platform.sim.clock.rings.clone();
    let platform = Bringup::new(platform, &options, bench.launch(None)).unwrap();
    let mut device = Device::start_with(platform, options).unwrap();
    common::connect_control(&mut device);
    (device, log, rings, media)
}

/// Review-r21 H4 end to end: with `--services --persist`, a Cortex Control Install of a
/// downloaded package is refused before its pre-steps: no `MasterMute`, no input mask, and
/// Control never sees the Updating state.
#[test]
fn a_control_install_in_bringup_mutes_nothing_and_masks_no_input() {
    use tetrad_control::codecs::{
        ControlMessage, MessageAction, UpdaterCommand, UpdaterMessage, UpdaterState,
    };
    let bench = Bench::new("control-install");
    let options = RunOptions {
        services: true,
        persist: true,
        preset: Some(LIGHT.into()),
        ..bench.options(Stage::Full, 600_000)
    };
    let (mut device, log, _rings, _media) = scripted_bringup(&bench, options);
    device.push(Event::Service(ServiceNotice::Download(Ok(()))));
    (0..20).for_each(|_| _ = device.step());
    let requests = log.borrow().requests.len();
    let install = UpdaterMessage {
        action: Some(MessageAction::Update.into()),
        command: Some(UpdaterCommand::Install.into()),
        ..Default::default()
    };
    common::host_send(&mut device, install);
    let refused = "bring-up refused an update download or install";
    let notices = &device.state().notices;
    assert!(
        notices.iter().any(|notice| notice.detail == refused),
        "the Install reached the refusal: {notices:?}"
    );
    let log = log.borrow();
    let mute = MessageType::MasterMute as u8;
    assert!(!log.kinds(requests).contains(&mute));
    assert!(!device.bridges().services.inputs_masked());
    let updating = Some(UpdaterState::Updating.into());
    let sent = common::control_sent(&log, requests);
    assert!(
        !sent
            .iter()
            .any(|m| matches!(m, ControlMessage::Updater(m) if m.state == updating)),
        "{sent:?}"
    );
}

/// Review r23 K7: bring-up without `--persist` refuses a Cortex Control cloud backup CREATE,
/// and Control gets its end at once (the failed UPDATE, then the resume of the suspended
/// link), not at the 30 min deadline.
#[test]
fn a_refused_control_cloud_backup_ends_at_once() {
    use tetrad_app::device::control::CommsCipher;
    use tetrad_control::codecs::{
        CloudBackupItem, CloudBackupMessage, ControlMessage, MessageAction, VersionMessage,
    };
    use tetrad_control::pipeline::Transform;
    use tetrad_dsp_protocol::hid::{HEADER_BYTES, REPORT_BYTES};
    let bench = Bench::new("cloud-refused");
    let options = RunOptions {
        services: true,
        ..bench.options(Stage::Full, 600_000)
    };
    let (mut device, log, rings, _media) = scripted_bringup(&bench, options);
    // The Version READ key exchange (fake vectors) installs the comms transform.
    let version = VersionMessage {
        action: Some(MessageAction::Read.into()),
        ..Default::default()
    };
    common::host_send(&mut device, version);
    let material = *b"fake vector B 16";
    let answer = encode(
        ResponseType::UsbMeterInput,
        0,
        &[[7; 16], material].concat(),
    );
    common::publish(
        &mut rings.0.borrow_mut(),
        CoreId::Dsp(DspCore::Core3),
        &answer,
    );
    (0..50).for_each(|_| _ = device.step());
    let cipher = CommsCipher::derive(&material);

    let mark = log.borrow().requests.len();
    let create = CloudBackupMessage {
        action: Some(MessageAction::Create.into()),
        backups: vec![CloudBackupItem {
            name: Some("Rig".into()),
            ..Default::default()
        }],
        ..Default::default()
    };
    common::host_send(&mut device, create);
    let mut reassembler = tetrad_control::Reassembler::new(tetrad_control::Limits::default());
    let log = log.borrow();
    let sent: Vec<ControlMessage> = (log.requests[mark..].iter())
        .filter(|sent| sent.kind() == 0x27 && sent.frame[2] == 0)
        .filter_map(|sent| {
            let report = &sent.frame[HEADER_BYTES..HEADER_BYTES + REPORT_BYTES];
            reassembler.push(report).unwrap()
        })
        .filter(|envelope| envelope.decompress == 0)
        .map(|mut envelope| {
            if envelope.transform != 0 {
                envelope.payload = cipher.decode(&envelope.payload).unwrap();
                envelope.transform = 0;
            }
            let limits = tetrad_control::link::LinkLimits::default().payload;
            ControlMessage::decode(&envelope, limits).unwrap().message
        })
        .collect();
    let failed = sent
        .iter()
        .position(|m| matches!(m, ControlMessage::CloudBackup(m) if m.error_code.is_some()));
    let resumed = sent.iter().position(
        |m| matches!(m, ControlMessage::SuspendConnection(m) if m.suspend == Some(false)),
    );
    assert!(
        matches!((failed, resumed), (Some(failed), Some(resumed)) if failed < resumed),
        "{sent:?}"
    );
}

/// Health r34 #5: with `--persist` but without `--services`, a Cortex Control licence refresh
/// (License UPDATE) is refused with a notice before the services bridge sees it.
#[test]
fn a_control_licence_refresh_needs_services() {
    use tetrad_control::codecs::{LicenseMessage, MessageAction};
    let bench = Bench::new("licence-refused");
    let options = RunOptions {
        persist: true,
        ..bench.options(Stage::Full, 600_000)
    };
    let (mut device, _log, _rings, _media) = scripted_bringup(&bench, options);
    let refresh = LicenseMessage {
        action: Some(MessageAction::Update.into()),
        ..Default::default()
    };
    common::host_send(&mut device, refresh);
    let notices = &device.state().notices;
    assert!(
        (notices.iter()).any(|notice| notice
            .detail
            .contains("services in a bring-up run without --services")),
        "{notices:?}"
    );
}

/// r21-control MR 6: without `--persist`, a capture save from Cortex Control (`save_info`)
/// or from the UI is refused before it reaches `zenui.db`. An EXP calibration save needs no
/// refusal: it is a settings write, which goes to the would-write folder.
#[test]
fn capture_saves_need_persist() {
    use tetrad_app::device::Effect;
    use tetrad_app::device::capture::NcRemote;
    use tetrad_app::device::ui::Handoff;
    use tetrad_capture::session::SaveInfo;
    use tetrad_ui::{CaptureDevice, CaptureInstrument, UiCommand};
    let bench = Bench::new("capture-save");
    let options = bench.options(Stage::Full, 60_000);
    let persist = RunOptions {
        persist: true,
        ..options.clone()
    };
    let remote = Effect::CaptureRemote(NcRemote::Save(SaveInfo {
        name: "Amp".into(),
        device: 1,
        instrument: 1,
    }));
    let ui = Handoff::Capture(UiCommand::SaveCapture {
        name: "Amp".into(),
        device: CaptureDevice::Amp,
        instrument: CaptureInstrument::Guitar,
    });
    let refused = Some("a capture save without --persist");
    assert_eq!(options.refusal(&remote), refused);
    assert_eq!(options.handoff_refusal(&ui), refused);
    assert_eq!(persist.refusal(&remote), None);
    assert_eq!(persist.handoff_refusal(&ui), None);
    let calibration = Effect::ExpressionCalibration {
        pedal: 0,
        calibrating: false,
    };
    assert_eq!(options.refusal(&calibration), None, "a would-write");
}

/// Review r14 S-6: without `--persist`, bring-up refuses every `zenui.db` write of the Virtual
/// Device Presets (edit, Save As, defaults), the Directory folder links and the quarantine
/// RESTORE (review r23 K5).
#[test]
fn device_preset_and_folder_link_writes_need_persist() {
    use tetrad_app::device::Effect;
    use tetrad_app::device::actions::DevicePresetEdit;
    use tetrad_storage::stock::ExpSwitch;
    let bench = Bench::new("vd-presets");
    let options = bench.options(Stage::Full, 60_000);
    let persist = RunOptions {
        persist: true,
        ..options.clone()
    };
    let switch = ExpSwitch {
        switch_type: 0,
        inverted: false,
        latch_emulation: false,
        delay_ms: 0,
    };
    let writes = [
        Action::DevicePreset(DevicePresetEdit::Delete(1)),
        Action::CreateDevicePreset {
            model: 1,
            name: "Mine".into(),
            switch,
            params: Vec::new(),
        },
        Action::SetDefaultParams {
            key: "1".into(),
            switch,
            params: Vec::new(),
        },
        Action::LinkMedia {
            folder: 2,
            products: vec!["cap".into()],
            linked: true,
        },
        Action::DuplicateMediaFolder { folder: 2 },
        Action::SetSavingDestination { folder: 2 },
        Action::PinModel {
            model: 1,
            pinned: true,
        },
        Action::RestoreQuarantinedCapture {
            store: tetrad_storage::stock::CaptureStore::User,
            id: "cap".into(),
        },
        Action::CopyPresetTo {
            from: "A".into(),
            slot: 0,
            to: "B".into(),
            to_slot: Some(3),
            remove: true,
        },
        Action::PastePresets {
            from: "A".into(),
            to: "A".into(),
            pairs: vec![(0, 3)],
            remove: false,
        },
        Action::SwapPresets {
            from: "A".into(),
            slot: 0,
            to: "B".into(),
            to_slot: 3,
        },
    ]
    .map(Effect::Action);
    for effect in &writes {
        assert_eq!(
            options.refusal(effect),
            Some("a library write without --persist"),
            "{effect:?}"
        );
        assert_eq!(persist.refusal(effect), None);
        assert_eq!(RunOptions::STOCK.refusal(effect), None);
    }
    // Save As into a given slot (Cortex Control preset CREATE with an index) is a preset save.
    let save_into = Effect::SavePresetInto {
        name: "Mine".into(),
        setlist: None,
        slot: 3,
        instrument: Default::default(),
    };
    assert_eq!(
        options.refusal(&save_into),
        Some("a preset save without --persist")
    );
    assert_eq!(persist.refusal(&save_into), None);
    // r47-control: a Control preset save with `instrument` is Edit Details of the loaded preset.
    let details = Effect::EditPresetDetails {
        name: "Mine".into(),
        instrument: tetrad_ui::PreferredInstrument::Bass,
    };
    assert_eq!(
        options.refusal(&details),
        Some("a preset save without --persist")
    );
    assert_eq!(persist.refusal(&details), None);
}

/// re-r31 §5: a bulk preset download writes setlist slots after cloud requests, so a bring-up
/// run refuses it without `--services` and without `--persist`. A stock run refuses nothing.
#[test]
fn a_bulk_preset_download_needs_services_and_persist() {
    use tetrad_app::device::ui::Handoff;
    let bench = Bench::new("download-presets");
    let full = bench.options(Stage::Full, 60_000);
    let services = RunOptions {
        services: true,
        ..full.clone()
    };
    let persist = RunOptions {
        persist: true,
        ..services.clone()
    };
    let download = Handoff::DownloadPresets {
        ids: vec!["p-1".into()],
        setlist: "My Presets".into(),
        to: tetrad_ui::PasteTarget::FirstEmpty,
        overwrite: false,
    };
    assert!(full.handoff_refusal(&download).is_some());
    assert_eq!(
        services.handoff_refusal(&download),
        Some("a setlist write without --persist")
    );
    assert_eq!(persist.handoff_refusal(&download), None);
    assert_eq!(RunOptions::STOCK.handoff_refusal(&download), None);
}

/// re-r44 §3 (O): with `--services --persist` but without `--control-cloud`, a bring-up run
/// gives the services no Cortex Control route (`Control::cloud_route`), and a call posted to
/// the forward link anyway fails at once: no ProductForward (29) reaches Control. With the
/// flag, the route is Control once the link is keyed and the KeepAlive says it is online
/// (`0x6e045c`).
#[test]
fn cloud_calls_go_over_control_only_with_control_cloud() {
    use tetrad_app::device::control::forward::ForwardHttp;
    use tetrad_control::codecs::{ControlMessage, KeepAliveMessage, MessageAction};
    use tetrad_services::http::{HttpClient, HttpError, HttpRequest, Method, Route};
    for control_cloud in [false, true] {
        let bench = Bench::new(&format!("control-cloud-{control_cloud}"));
        let options = RunOptions {
            services: true,
            persist: true,
            control_cloud,
            ..bench.options(Stage::Full, 600_000)
        };
        let (mut device, log, rings, _media) = scripted_bringup(&bench, options);
        if control_cloud {
            key_control(&mut device, &rings);
        }
        assert!(device.bridges().control.cloud_route().is_none(), "offline");
        let online = KeepAliveMessage {
            action: Some(MessageAction::Update.into()),
            is_online: Some(true),
            ..Default::default()
        };
        common::host_send(&mut device, online);
        let route = device.bridges().control.cloud_route();
        assert_eq!(route.is_some(), control_cloud);
        if control_cloud {
            continue;
        }
        let mark = log.borrow().requests.len();
        let link = device.bridges().control.forward_link();
        let worker = std::thread::spawn(move || {
            let request = HttpRequest::new(
                Method::Get,
                "https://example.invalid/user/products",
                Route::Product,
                Duration::from_secs(30),
            );
            ForwardHttp::new(link, MemFiles::default()).send(&request)
        });
        for _ in 0..2000 {
            if worker.is_finished() {
                break;
            }
            device.step();
            std::thread::yield_now();
        }
        assert!(worker.is_finished(), "the forward still waits for Control");
        let Err(HttpError::Transport(error)) = worker.join().unwrap() else {
            panic!("the forward is refused");
        };
        assert!(error.contains("--control-cloud"), "{error}");
        let sent = common::control_sent(&log.borrow(), mark);
        assert!(
            !(sent.iter()).any(|m| matches!(m, ControlMessage::ProductForward(_))),
            "{sent:?}"
        );
    }
}

/// The Version READ key exchange (fake vectors) of a bring-up device: the DSP answer `0x17`
/// installs the link transform, which the Control cloud route needs (review-r45 M4).
fn key_control(
    device: &mut Device<Bringup<Scripted>>,
    rings: &common::Window,
) -> tetrad_app::device::control::CommsCipher {
    use tetrad_control::codecs::{MessageAction, VersionMessage};
    let version = VersionMessage {
        action: Some(MessageAction::Read.into()),
        ..Default::default()
    };
    common::host_send(device, version);
    let material = *b"fake vector B 16";
    let answer = encode(
        ResponseType::UsbMeterInput,
        0,
        &[[7; 16], material].concat(),
    );
    common::publish(
        &mut rings.0.borrow_mut(),
        CoreId::Dsp(DspCore::Core3),
        &answer,
    );
    (0..50).for_each(|_| _ = device.step());
    tetrad_app::device::control::CommsCipher::derive(&material)
}

/// Review-r45 M8: with `--services --control-cloud` and no `--persist`, Cortex Control keyed,
/// connected and online (the cloud route is Control), Control's own cloud writes are each
/// refused by bring-up: CloudBackup (41) CREATE and UPDATE, CloudProduct (46) DELETE and
/// UPLOAD (a setlist preset with its `from_folder`), the licence refresh (58 UPDATE). No
/// ProductForward, BackupsForward or LogsForward (29, 30, 31) leaves.
#[test]
fn control_cloud_writes_are_refused_without_persist() {
    use tetrad_control::codecs::{
        CloudBackupItem, CloudBackupMessage, CloudProductMessage, ControlMessage, KeepAliveMessage,
        LicenseMessage, MessageAction,
    };
    use tetrad_control::pipeline::Transform;
    use tetrad_dsp_protocol::hid::{HEADER_BYTES, REPORT_BYTES};
    let bench = Bench::new("control-cloud-writes");
    let options = RunOptions {
        services: true,
        control_cloud: true,
        ..bench.options(Stage::Full, 600_000)
    };
    options.validate().unwrap();
    let (mut device, log, rings, _media) = scripted_bringup(&bench, options);
    let cipher = key_control(&mut device, &rings);
    let online = KeepAliveMessage {
        action: Some(MessageAction::Update.into()),
        is_online: Some(true),
        ..Default::default()
    };
    common::host_send(&mut device, online);
    assert!(
        device.bridges().control.cloud_route().is_some(),
        "route is Control"
    );
    let mark = log.borrow().requests.len();
    let backup = |action: MessageAction| CloudBackupMessage {
        action: Some(action.into()),
        backups: vec![CloudBackupItem {
            name: Some("Rig".into()),
            ..Default::default()
        }],
        ..Default::default()
    };
    let product = |action: MessageAction, key: &str, from: Option<&str>| CloudProductMessage {
        action: Some(action.into()),
        product_keys: vec![key.to_owned()],
        product_type: Some(0),
        from_folder: from.map(str::to_owned),
        ..Default::default()
    };
    let setlist = "/media/p4/Presets/My Presets";
    let licence = LicenseMessage {
        action: Some(MessageAction::Update.into()),
        ..Default::default()
    };
    // Message types alternate: an exact repeat of the last notice is not kept again.
    let writes: [ControlMessage; 5] = [
        backup(MessageAction::Create).into(),
        product(MessageAction::Delete, "p-1", None).into(),
        backup(MessageAction::Update).into(),
        licence.into(),
        product(MessageAction::Upload, LIGHT, Some(setlist)).into(),
    ];
    for write in writes {
        common::host_send(&mut device, write);
    }
    (0..200).for_each(|_| _ = device.step());
    let notices: Vec<&str> = (device.state().notices.iter())
        .map(|notice| notice.detail.as_str())
        .collect();
    let refused = (notices.iter())
        .filter(|notice| notice.contains("refused a cloud account write without --persist"))
        .count();
    assert_eq!(refused, 5, "{notices:#?}");
    let mut reassembler = tetrad_control::Reassembler::new(tetrad_control::Limits::default());
    let log = log.borrow();
    let sent: Vec<ControlMessage> = (log.requests[mark..].iter())
        .filter(|sent| sent.kind() == 0x27 && sent.frame[2] == 0)
        .filter_map(|sent| {
            let report = &sent.frame[HEADER_BYTES..HEADER_BYTES + REPORT_BYTES];
            reassembler.push(report).unwrap()
        })
        .filter(|envelope| envelope.decompress == 0)
        .filter_map(|mut envelope| {
            if envelope.transform != 0 {
                envelope.payload = cipher.decode(&envelope.payload).unwrap();
                envelope.transform = 0;
            }
            let limits = tetrad_control::link::LinkLimits::default().payload;
            ControlMessage::decode(&envelope, limits)
                .ok()
                .map(|d| d.message)
        })
        .collect();
    let forwards: Vec<_> = (sent.iter())
        .filter(|m| {
            matches!(
                m,
                ControlMessage::ProductForward(_)
                    | ControlMessage::BackupsForward(_)
                    | ControlMessage::LogsForward(_)
            )
        })
        .collect();
    assert!(forwards.is_empty(), "{forwards:?}");
}

//! The simulated QC platform that the device integration tests share: the crates' own fakes
//! (tetrad-loader `RecordingBus`/`SimSharc`, tetrad-hal `RecordingHal`, tetrad-services fakes), a
//! scripted DSP on the real ring layout, in-memory settings and a temp-dir stock library. It
//! boots through the stock start-up order and answers every request the deploy and redeploy
//! paths wait for. All bytes are invented; keys are fixed test vectors delivered by the
//! scripted DSP.

// Each test crate uses a different part of this module.
#![allow(dead_code)]

use std::cell::{Cell, RefCell, RefMut};
use std::collections::BTreeMap;
use std::fs;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, SystemTime};

use qc_format::blob::{LinkRange, ModuleBlob, Section, SectionBytes};
use qc_format::crypto::crypt;
use qc_format::diff::{DiffFile, DiffRecord};
use tetrad_app::device::platform::{
    DspFile, DspWindows, Input, Marker, Platform, ServiceBackends, ServiceIo, SharcFirmware,
    Soc2Firmware, Soc2Pair, StorageRoots, TouchSample,
};
use tetrad_app::device::preset::SpecsCatalog;
use tetrad_app::device::services::{ServiceJob, ServiceNotice};
use tetrad_app::device::ui::Ui;
use tetrad_app::device::{Device, Event, Notice};
use tetrad_capture::session::SharcMemory;
use tetrad_catalog::{Catalog, Specs};
use tetrad_control::codecs::{
    ConnectionMessage, ControlMessage, CortexMessageType, KeepAliveMessage, MessageAction,
    ModelRepoMessage, SceneMessage, VersionMessage,
};
use tetrad_control::link::LinkLimits;
use tetrad_control::{Envelope, Limits, Reassembler};
use tetrad_dsp_host::drain::MainLoop;
use tetrad_dsp_host::model::{CoreWindows, ModelMemory};
use tetrad_dsp_host::session::{Clock, RECEIVE_ORDER};
use tetrad_dsp_host::transport::{MESSAGE_MEMORY_BYTES, RingLayout};
use tetrad_dsp_host::{MemoryError, SharedMemory};
use tetrad_dsp_protocol::address::Address;
use tetrad_dsp_protocol::hid::{HEADER_BYTES, REPORT_BYTES};
use tetrad_dsp_protocol::responses::{ResponseType, encode};
use tetrad_dsp_protocol::{CoreId, DspCore};
use tetrad_hal::hal::{
    Command as HalCommand, Event as HalEvent, Hal, HalError, Query, Reading, RecordingHal,
};
use tetrad_loader::PhysAddr;
use tetrad_loader::booter::StockBooter;
use tetrad_loader::exec::{AccessError, CoreControl, PhysicalMemory};
use tetrad_loader::fake::{RecordingBus, SimSharc};
use tetrad_loader::intent::{CoreAction, LoaderCore};
use tetrad_loader::plan::Variant;
use tetrad_preset::{Model, ParamValue, Preset, Slot};
use tetrad_services::exec::{
    CancelToken, Command, CommandError, CommandRunner, Files, NetError, NetIo, Output,
};
use tetrad_services::fake::{FakeChild, MemFiles, ScriptedHttp, ScriptedNet, ScriptedRunner};
use tetrad_services::http::{HttpClient, HttpError, HttpRequest, HttpResponse, Method};
use tetrad_services::wifi::{LinkError, SupplicantLink};
use tetrad_settings::{SettingsFileId, SettingsStorage};
use tetrad_storage::media::{MediaRoot, SETLIST_SLOTS, encode_setlist};
use tetrad_ui::Control;
use tetrad_watchdog::power::{PowerBackend, PowerStep, StepStatus};
use tetrad_watchdog::splash::{SplashCommand, SplashQueue};

pub const REPO_KEY: [u8; 16] = [0x41; 16];
pub const BLOB_KEY: [u8; 16] = [0x42; 16];
pub const SOC2_BUFFER: u32 = 0x9000_0000;
/// Core window base: the first code parcel `0x1c0000` transfers to byte `0x380000`.
pub const CORE_WINDOW_BASE: u32 = 0x38_0000;
pub const CODE: [u8; 4] = [0xaa, 0xbb, 0xcc, 0xdd];

/// A drive (`Light`), a cab whose `ir selector` the IR picker edits, the two driver models
/// that write the preset back (Dual Delay 6012 links FEEDBACK L/R by LINK FBACK; Freeze 26001
/// takes the footswitch), and the hidden models every preset holds. The NC2 preset models
/// (`0x51b7e8`): player 14001, FX loop send 13000 and the default capture cab 12001; the
/// lane MUTE/SOLO (23000 p2/p3), mixer p2 and gate INPUT GAIN (28000 p3) that the NC2 routes
/// edit. Tempo has all 23
/// parameters for the metronome resend. The Minivoicer 18007 (25 parameters) runs the MIDI
/// voice driver.
pub const MODELS: &str = "<Models>\
<Category id='0' name='Drive'><Model id='1' name='Light' blob='light'>\
<Parameter name='GAIN' type='float' min='0' max='10' defaultValue='5'/></Model></Category>\
<Category id='6' name='Delay'><Model id='6012' name='Dual Delay' blob='light'>\
<Parameter name='P0' type='float' defaultValue='0'/><Parameter name='P1' type='float' defaultValue='0'/>\
<Parameter name='P2' type='float' defaultValue='0'/><Parameter name='P3' type='float' defaultValue='0'/>\
<Parameter name='FEEDBACK L' type='float' defaultValue='0.25'/>\
<Parameter name='P5' type='float' defaultValue='0'/><Parameter name='P6' type='float' defaultValue='0'/>\
<Parameter name='P7' type='float' defaultValue='0'/><Parameter name='P8' type='float' defaultValue='0'/>\
<Parameter name='FEEDBACK R' type='float' defaultValue='0.25'/>\
<Parameter name='P10' type='float' defaultValue='0'/><Parameter name='P11' type='float' defaultValue='0'/>\
<Parameter name='P12' type='float' defaultValue='0'/><Parameter name='P13' type='float' defaultValue='0'/>\
<Parameter name='LINK FBACK' type='switch' min='0' max='1' defaultValue='0'/></Model>\
<Model id='6031' name='Arpeggio Delay' blob='light'>\
<Parameter name='MIX' type='float' defaultValue='0.5'/>\
<Parameter name='SYNC' type='switch' min='0' max='1' defaultValue='0'/>\
<Parameter name='P2' type='float' defaultValue='0'/>\
<Parameter name='SCALE' type='rotarySwitch' min='0' max='4' steps='5' defaultValue='0'/>\
<Parameter name='GLISSANDO' type='float' defaultValue='0'/>\
<Parameter name='ACTIVE' type='switch' min='0' max='1' defaultValue='1'/>\
<Parameter name='SEMITONES' type='rotarySwitch' min='-12' max='12' steps='25' defaultValue='0'/>\
<Parameter name='INTERVAL' type='rotarySwitch' min='0' max='14' steps='15' defaultValue='7'/>\
<Parameter name='DELAY TIME' type='float' defaultValue='0.5'/>\
<Parameter name='SYNC NOTE' type='rotarySwitch' min='0' max='10' steps='11' defaultValue='5'/>\
</Model></Category>\
<Category id='5' name='Dynamics'><Model id='5026' name='Comp' blob='light'>\
<Parameter name='P0' type='float' defaultValue='0'/><Parameter name='P1' type='float' defaultValue='0'/>\
<Parameter name='GR' type='grMeter' min='-50' max='0' defaultValue='0'/></Model></Category>\
<Category id='16' name='Utility'><Model id='16007' name='Adaptive Gate' blob='light' sidechain='1'>\
<Parameter name='THRESHOLD' type='float' min='-96' max='0' defaultValue='-60'/>\
<Parameter name='SOURCE' type='rotarySwitch' min='0' max='44' defaultValue='0' dynamic='true' \
stepNames='Off'/></Model></Category>\
<Category id='26' name='Utility'><Model id='26001' name='Freeze' blob='light'>\
<Parameter name='ON/OFF' type='switch' min='0' max='1' defaultValue='0'/>\
<Parameter name='MOMENTARY' type='switch' min='0' max='1' defaultValue='0'/></Model></Category>\
<Category id='18' name='Synth'><Model id='18007' name='Minivoicer' blob='light'>\
<Parameter name='P0' type='float' defaultValue='0'/><Parameter name='P1' type='float' defaultValue='0'/><Parameter name='P2' type='float' defaultValue='0'/>\
<Parameter name='P3' type='float' defaultValue='0'/><Parameter name='P4' type='float' defaultValue='0'/><Parameter name='P5' type='float' defaultValue='0'/>\
<Parameter name='P6' type='float' defaultValue='0'/><Parameter name='P7' type='float' defaultValue='0'/><Parameter name='P8' type='float' defaultValue='0'/>\
<Parameter name='P9' type='float' defaultValue='0'/><Parameter name='P10' type='float' defaultValue='0'/><Parameter name='P11' type='float' defaultValue='0'/>\
<Parameter name='P12' type='float' defaultValue='0'/><Parameter name='P13' type='float' defaultValue='0'/><Parameter name='P14' type='float' defaultValue='0'/>\
<Parameter name='P15' type='float' defaultValue='0'/><Parameter name='P16' type='float' defaultValue='0'/><Parameter name='P17' type='float' defaultValue='0'/>\
<Parameter name='P18' type='float' defaultValue='0'/><Parameter name='P19' type='float' defaultValue='0'/><Parameter name='P20' type='float' defaultValue='0'/>\
<Parameter name='P21' type='float' defaultValue='0'/><Parameter name='P22' type='float' defaultValue='0'/><Parameter name='P23' type='float' defaultValue='0'/>\
<Parameter name='P24' type='float' defaultValue='0'/></Model></Category>\
<Category id='10' name='Splitter' hidden='true'><Model id='10004' name='Splitter'>\
<Parameter name='TYPE' type='switch' min='0' max='2' defaultValue='0'/></Model></Category>\
<Category id='12' name='Cab'><Model id='12000' name='Cab' blob='light'>\
<Parameter name='LEVEL' type='float' min='0' max='10' defaultValue='5'/>\
<Parameter name='ir selector' type='string' defaultValue='IR'/></Model>\
<Model id='12001' name='Capture Cab' blob='light'>\
<Parameter name='LEVEL' type='float' min='0' max='10' defaultValue='5'/>\
<Parameter name='ir selector' type='string' defaultValue='IR'/></Model></Category>\
<Category id='13' name='FX Loop'><Model id='13000' name='Send' blob='light'>\
<Parameter name='LEVEL' type='float' min='0' max='1' defaultValue='0.5'/></Model></Category>\
<Category id='14' name='Capture'><Model id='14001' name='Capture 2' blob='light'>\
<Parameter name='GAIN' type='float' min='0' max='1' defaultValue='0.5'/></Model></Category>\
<Category id='11' name='Mixer' hidden='true'><Model id='11000' name='Mixer'>\
<Parameter name='LEVEL A' type='float' min='MIN_MIXER_DB' max='MAX_MIXER_DB' defaultValue='0'/>\
<Parameter name='P1' type='float' min='0' max='1' defaultValue='0.5'/>\
<Parameter name='LEVEL B' type='float' min='MIN_MIXER_DB' max='MAX_MIXER_DB' defaultValue='0'/>\
</Model></Category>\
<Category id='23' name='Lane Output' hidden='true'><Model id='23000' name='Lane Output'>\
<Parameter name='VOLUME' type='float' min='0' max='1' defaultValue='0.76923'/>\
<Parameter name='PAN' type='float' min='0' max='1' defaultValue='0.5'/>\
<Parameter name='MUTE' type='switch' min='0' max='1' defaultValue='0'/>\
<Parameter name='SOLO' type='switch' min='0' max='1' defaultValue='0'/></Model></Category>\
<Category id='25' name='Tempo' hidden='true'><Model id='25000' name='Tempo'>\
<Parameter name='TEMPO' type='float' min='40' max='240' defaultValue='120'/>\
<Parameter name='SCOPE' type='switch' min='0' max='1' defaultValue='0'/>\
<Parameter name='LED' type='switch' min='0' max='1' defaultValue='1'/>\
<Parameter name='LEVEL' type='float' min='-60' max='0' defaultValue='-12'/>\
<Parameter name='ENABLED' type='switch' min='0' max='1' defaultValue='0'/>\
<Parameter name='PAN' type='float' min='-1' max='1' defaultValue='0'/>\
<Parameter name='SIGNATURE' type='switch' min='0' max='20' defaultValue='2'/>\
<Parameter name='SUBDIVISION' type='switch' min='0' max='3' defaultValue='0'/>\
<Parameter name='P8' type='float' min='0' max='1' defaultValue='0'/>\
<Parameter name='DESTINATION' type='switch' min='0' max='4' defaultValue='0'/>\
<Parameter name='P10' type='float' min='0' max='1' defaultValue='0'/>\
<Parameter name='P11' type='float' min='0' max='1' defaultValue='0'/>\
<Parameter name='P12' type='float' min='0' max='1' defaultValue='0'/>\
<Parameter name='P13' type='float' min='0' max='1' defaultValue='0'/>\
<Parameter name='P14' type='float' min='0' max='1' defaultValue='0'/>\
<Parameter name='P15' type='float' min='0' max='1' defaultValue='0'/>\
<Parameter name='P16' type='float' min='0' max='1' defaultValue='0'/>\
<Parameter name='P17' type='float' min='0' max='1' defaultValue='0'/>\
<Parameter name='P18' type='float' min='0' max='1' defaultValue='0'/>\
<Parameter name='P19' type='float' min='0' max='1' defaultValue='0'/>\
<Parameter name='P20' type='float' min='0' max='1' defaultValue='0'/>\
<Parameter name='P21' type='float' min='0' max='1' defaultValue='0'/>\
<Parameter name='P22' type='float' min='0' max='1' defaultValue='0'/>\
</Model></Category>\
<Category id='28' name='Input Gate' hidden='true'><Model id='28000' name='Input Gate'>\
<Parameter name='THRESHOLD' type='float' min='-96' max='0' defaultValue='-60'/>\
<Parameter name='P1' type='float' min='0' max='1' defaultValue='0'/>\
<Parameter name='P2' type='float' min='0' max='1' defaultValue='0'/>\
<Parameter name='INPUT GAIN' type='float' min='-24' max='24' defaultValue='0'/></Model></Category>\
</Models>";

pub const REPORT: &str = "<ModuleReport>\
<Kernel core='0' cpu='0' dm_heap='0' pm_heap='0' sd_heap='0'/>\
<Kernel core='1' cpu='2' dm_heap='0' pm_heap='0' sd_heap='0'/>\
<Kernel core='2' cpu='0' dm_heap='0' pm_heap='0' sd_heap='0'/>\
<Kernel core='3' cpu='0' dm_heap='0' pm_heap='0' sd_heap='0'/>\
<Module id='1' name='Light' blob_name='light' category='0' dm_heap='100' pm_heap='10' \
sd_heap='0' dma='0' dm='10' pm='0' sw='100' nw='0' Latency='16'>\
<Test name='Stimulus High' avg='10'/></Module>\
<Module id='12000' name='Cab' blob_name='light' category='12' dm_heap='100' pm_heap='10' \
sd_heap='0' dma='0' dm='10' pm='0' sw='100' nw='0' Latency='16'>\
<Test name='Stimulus High' avg='10'/></Module>\
<Module id='6012' name='Dual Delay' blob_name='light' category='6' dm_heap='100' pm_heap='10' \
sd_heap='0' dma='0' dm='10' pm='0' sw='100' nw='0' Latency='16'>\
<Test name='Stimulus High' avg='10'/></Module>\
<Module id='6031' name='Arpeggio Delay' blob_name='light' category='6' dm_heap='100' pm_heap='10' \
sd_heap='0' dma='0' dm='10' pm='0' sw='100' nw='0' Latency='16'>\
<Test name='Stimulus High' avg='10'/></Module>\
<Module id='12001' name='Capture Cab' blob_name='light' category='12' dm_heap='100' pm_heap='10' \
sd_heap='0' dma='0' dm='10' pm='0' sw='100' nw='0' Latency='16'>\
<Test name='Stimulus High' avg='10'/></Module>\
<Module id='13000' name='Send' blob_name='light' category='13' dm_heap='100' pm_heap='10' \
sd_heap='0' dma='0' dm='10' pm='0' sw='100' nw='0' Latency='16'>\
<Test name='Stimulus High' avg='10'/></Module>\
<Module id='14001' name='Capture 2' blob_name='light' category='14' dm_heap='100' pm_heap='10' \
sd_heap='0' dma='0' dm='10' pm='0' sw='100' nw='0' Latency='16'>\
<Test name='Stimulus High' avg='10'/></Module>\
<Module id='5026' name='Comp' blob_name='light' category='5' dm_heap='100' pm_heap='10' \
sd_heap='0' dma='0' dm='10' pm='0' sw='100' nw='0' Latency='16'>\
<Test name='Stimulus High' avg='10'/></Module>\
<Module id='16007' name='Adaptive Gate' blob_name='light' category='16' dm_heap='100' \
pm_heap='10' sd_heap='0' dma='0' dm='10' pm='0' sw='100' nw='0' Latency='16'>\
<Test name='Stimulus High' avg='10'/></Module>\
<Module id='26001' name='Freeze' blob_name='light' category='26' dm_heap='100' pm_heap='10' \
sd_heap='0' dma='0' dm='10' pm='0' sw='100' nw='0' Latency='16'>\
<Test name='Stimulus High' avg='10'/></Module>\
<Module id='18007' name='Minivoicer' blob_name='light' category='18' dm_heap='100' pm_heap='10' \
sd_heap='0' dma='0' dm='10' pm='0' sw='100' nw='0' Latency='16'>\
<Test name='Stimulus High' avg='10'/></Module>\
</ModuleReport>";

/// Recovered stock DDL (ZenUI `0x00a453cc`, `0x00a4693c`, `0x00a45328`) at schema 410.1.
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

/// The factory tables of `factory.db` (factory DDL `0x00a4668c`, `0x00a46b64`).
const FACTORY_DDL: &str = "
CREATE TABLE product ( id TEXT, type INT, readonly INT, rawdata BLOB, metadata BLOB, PRIMARY KEY(id));
CREATE TABLE folder ( id INTEGER PRIMARY KEY AUTOINCREMENT, pdType INT, folderType INT, name TEXT, parent INTEGER, readonly INT);
CREATE TABLE folder_content ( folderId INTEGER, productId TEXT, FOREIGN KEY(folderId) REFERENCES folder(id), PRIMARY KEY(folderId,productId));
CREATE TABLE model_presets ( id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT, hash INT, expSwitchType INT, expSwitchInverted INT, expSwitchLatchEmulation INT, expSwitchDelayMs INT);
CREATE TABLE model_preset_parameters (presetId INT, paramIndex INT, value TEXT, exp INT, expTrimMax REAL, expTrimMin REAL, FOREIGN KEY (presetId) REFERENCES model_presets(id), PRIMARY KEY (presetId, paramIndex));
CREATE TABLE schema_version ( major INTEGER, minor INTEGER);
INSERT INTO schema_version VALUES (410, 1);
";

/// The catalog and blobs the simulated DSP serves.
#[derive(Clone, Copy)]
pub struct Fixture {
    pub models: &'static str,
    pub report: &'static str,
    /// The decrypted blob for a blob name.
    pub blob: fn(&str) -> ModuleBlob,
    /// Blob names with a (empty) diff record.
    pub diffs: &'static [&'static str],
}

impl Fixture {
    pub fn specs(&self) -> Specs {
        Specs::resolve(&Catalog::parse(self.models, self.report).unwrap()).unwrap()
    }
}

/// [`MODELS`] and [`REPORT`]: every model runs the `light` blob.
pub const STOCK: Fixture = Fixture {
    models: MODELS,
    report: REPORT,
    blob: |name| {
        assert_eq!(name, "light", "unexpected blob");
        light_blob()
    },
    diffs: &["light"],
};

/// One request frame the scripted DSP consumed.
#[derive(Debug, Clone)]
pub struct Sent {
    pub at: Duration,
    pub ring: CoreId,
    pub frame: Vec<u8>,
}

impl Sent {
    pub fn kind(&self) -> u8 {
        self.frame[0]
    }

    /// The word at `+0x10`, if the frame has one.
    pub fn word(&self) -> Option<u32> {
        let word = self.frame.get(16..20)?;
        Some(u32::from_le_bytes(word.try_into().unwrap()))
    }
}

/// Everything the simulated hardware observed, in order, with simulated time.
#[derive(Debug, Default)]
pub struct Log {
    pub requests: Vec<Sent>,
    pub splash: Vec<i32>,
    pub settings_writes: Vec<SettingsFileId>,
    pub notices: Vec<Notice>,
    /// Byte length of every presented frame (800×480 RGB8 each).
    pub frames: Vec<usize>,
    /// `Platform::forget_display` calls (the whole frame is written again).
    pub display_forgets: u32,
    pub midi: Vec<Vec<u8>>,
    /// DSP requests, splash values, HAL commands and power steps in one order.
    pub trace: Vec<Mark>,
}

impl Log {
    /// Request types from `mark` on.
    pub fn kinds(&self, mark: usize) -> Vec<u8> {
        self.requests[mark..].iter().map(Sent::kind).collect()
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Mark {
    /// Type and word at +0x10.
    Dsp(u8, Option<u32>),
    Splash(i32),
    Hal(HalCommand),
    Power(PowerStep),
}

/// One mapped DSP window.
#[derive(Clone)]
pub struct Window(pub Rc<RefCell<Vec<u8>>>);

impl Window {
    pub fn new(bytes: usize) -> Self {
        Self(Rc::new(RefCell::new(vec![0; bytes])))
    }
}

impl SharedMemory for Window {
    fn len(&self) -> usize {
        self.0.borrow().len()
    }
    fn read(&self, offset: usize, buf: &mut [u8]) -> Result<(), MemoryError> {
        let memory = self.0.borrow();
        let window = memory.len();
        let source = memory
            .get(offset..offset + buf.len())
            .ok_or(MemoryError::OutOfBounds {
                offset,
                len: buf.len(),
                window,
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
        if !offset.is_multiple_of(4) {
            return Err(MemoryError::Misaligned(offset));
        }
        self.read(offset, &mut word)?;
        Ok(u32::from_le_bytes(word))
    }
    fn write_u32(&mut self, offset: usize, value: u32) -> Result<(), MemoryError> {
        if !offset.is_multiple_of(4) {
            return Err(MemoryError::Misaligned(offset));
        }
        self.write(offset, &value.to_le_bytes())
    }
    fn acquire_barrier(&mut self) {}
    fn publish_barrier(&mut self) {}
}

/// A ring and frame the scripted DSP leaves unread while set.
pub type Hold = fn(CoreId, &[u8]) -> bool;

/// Simulated time. Every sleep lets the scripted DSP serve its rings, as the real SHARC
/// would while ARM waits.
#[derive(Clone)]
pub struct SimClock {
    now: Rc<RefCell<Duration>>,
    pub rings: Window,
    log: Rc<RefCell<Log>>,
    /// Sends that wait for this frame, or for a slot behind it, time out.
    pub hold: Rc<Cell<Option<Hold>>>,
    /// The hold ends at this simulated time, if set.
    pub hold_until: Rc<Cell<Option<Duration>>>,
}

impl Clock for SimClock {
    fn now(&self) -> Duration {
        *self.now.borrow()
    }
    fn sleep(&mut self, duration: Duration) {
        *self.now.borrow_mut() += duration;
        let now = self.now();
        let held = |ring, frame: &[u8]| {
            self.hold.get().is_some_and(|hold| hold(ring, frame))
                && self.hold_until.get().is_none_or(|until| now < until)
        };
        let mut memory = self.rings.0.borrow_mut();
        for ring in RECEIVE_ORDER {
            let out = RingLayout::outgoing(ring);
            while memory[out.consumer_offset] != memory[out.producer_offset] {
                let slot = (memory[out.consumer_offset] + 1) % out.slot_count;
                let at = out.length_offset + usize::from(slot) * 4;
                let len = u32::from_le_bytes(memory[at..at + 4].try_into().unwrap()) as usize;
                let at = out.payload_offset + usize::from(slot) * out.slot_bytes;
                let frame = memory[at..at + len].to_vec();
                if held(ring, &frame) {
                    break;
                }
                memory[out.consumer_offset] = slot;
                for (to, response) in answer(ring, &frame) {
                    publish(&mut memory, to, &response);
                }
                let sent = Sent {
                    at: now,
                    ring,
                    frame,
                };
                let mut log = self.log.borrow_mut();
                log.trace.push(Mark::Dsp(sent.kind(), sent.word()));
                log.requests.push(sent);
            }
        }
    }
}

impl SimClock {
    /// Lets the scripted DSP read what was sent so far, then records `mark` after it.
    fn record(&self, mark: Mark) -> RefMut<'_, Log> {
        self.clone().sleep(Duration::ZERO);
        let mut log = self.log.borrow_mut();
        log.trace.push(mark);
        log
    }
}

/// The scripted SHARC side: boot keys after the handshake, the SOC2 buffer and status, and
/// the run-list-cleared acknowledgement (`0x1c`) for every `0x1e` DeleteAllModels, at boot
/// and on each redeploy.
fn answer(ring: CoreId, request: &[u8]) -> Vec<(CoreId, Vec<u8>)> {
    let core0 = CoreId::Dsp(DspCore::Core0);
    let word = |value: u32| value.to_le_bytes().to_vec();
    match request[0] {
        0x57 => [
            (ResponseType::Vector24, REPO_KEY),
            (ResponseType::Vector25, BLOB_KEY),
            (ResponseType::Vector27, [0x43; 16]),
        ]
        .map(|(kind, key)| (core0, encode(kind, 0, &key)))
        .to_vec(),
        0x12 => vec![(
            core0,
            encode(ResponseType::SlaveBootBufferAddress, 0, &word(SOC2_BUFFER)),
        )],
        0x13 => vec![(core0, encode(ResponseType::SlaveBootResponse, 0, &word(0)))],
        // Word 0 names the SOC's first (0) or second (1) core (`Dispatcher::run_list_core`);
        // the QC sends 12 body bytes with word 2 = 1.
        0x1e => {
            let words = [u32::from(request[1] & 1), 0, 1];
            let body: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
            vec![(ring, encode(ResponseType::Word1c, 0, &body))]
        }
        _ => Vec::new(),
    }
}

/// Puts `bytes` in the next slot of `ring`'s incoming (DSP → ARM) queue.
pub fn publish(memory: &mut [u8], ring: CoreId, bytes: &[u8]) {
    let layout = RingLayout::incoming(ring);
    let slot = (memory[layout.producer_offset] + 1) % layout.slot_count;
    let at = layout.payload_offset + usize::from(slot) * layout.slot_bytes;
    memory[at..at + bytes.len()].copy_from_slice(bytes);
    let at = layout.length_offset + usize::from(slot) * 4;
    memory[at..at + 4].copy_from_slice(&(bytes.len() as u32).to_le_bytes());
    memory[layout.producer_offset] = slot;
}

/// The loader bus, shared so the test can inspect what each simulated core received.
#[derive(Clone)]
pub struct Bus(pub Rc<RefCell<RecordingBus<Vec<SimSharc>>>>);

impl PhysicalMemory for Bus {
    fn read_u32(&mut self, address: PhysAddr) -> Result<u32, AccessError> {
        self.0.borrow_mut().read_u32(address)
    }
    fn write_u32(&mut self, address: PhysAddr, value: u32) -> Result<(), AccessError> {
        self.0.borrow_mut().write_u32(address, value)
    }
    fn write_u16(&mut self, address: PhysAddr, value: u16) -> Result<(), AccessError> {
        self.0.borrow_mut().write_u16(address, value)
    }
    fn write_bytes(&mut self, address: PhysAddr, bytes: &[u8]) -> Result<(), AccessError> {
        self.0.borrow_mut().write_bytes(address, bytes)
    }
    fn now(&self) -> Duration {
        self.0.borrow().now()
    }
    fn delay(&mut self, duration: Duration) {
        self.0.borrow_mut().delay(duration);
    }
}

impl CoreControl for Bus {
    fn control(&mut self, core: LoaderCore, action: CoreAction) -> Result<(), AccessError> {
        self.0.borrow_mut().control(core, action)
    }
}

pub struct SimHal {
    hal: RecordingHal,
    clock: SimClock,
}

impl Hal for SimHal {
    fn apply(&mut self, command: HalCommand) -> Result<(), HalError> {
        self.clock.record(Mark::Hal(command));
        self.hal.apply(command)
    }
    fn read(&mut self, query: Query) -> Result<Reading, HalError> {
        self.hal.read(query)
    }
    fn cached(&self, control: tetrad_hal::zenjack::Control) -> Option<tetrad_hal::zenjack::Value> {
        self.hal.cached(control)
    }
    fn poll_events(&mut self, out: &mut Vec<HalEvent>) -> Result<(), HalError> {
        self.hal.poll_events(out)
    }
}

/// Splash queue and power backend.
pub struct Recorder(SimClock);

impl SplashQueue for Recorder {
    type Error = std::io::Error;
    fn send(&mut self, command: SplashCommand) -> Result<(), Self::Error> {
        let value = command.value();
        self.0.record(Mark::Splash(value)).splash.push(value);
        Ok(())
    }
}

impl PowerBackend for Recorder {
    type Error = std::io::Error;
    fn perform(&mut self, step: PowerStep) -> Result<StepStatus, Self::Error> {
        self.0.record(Mark::Power(step));
        Ok(StepStatus::Success)
    }
}

#[derive(Default)]
pub struct Settings {
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
        self.files.remove(&file);
        Ok(())
    }
}

/// The made-up network and cloud of [`Sim::cloud`]. Off (the default), the platform has no
/// supplicant, no network and no HTTP answer. Online, it answers the Wi-Fi bring-up, the
/// supplicant (one saved network "Home"), DHCP (192.0.2.40/24), the internet probe and NTP
/// ([`CLOUD_TIME`]); each HTTP request gets the newest [`Route`] that matches it.
#[derive(Default)]
pub struct Cloud {
    pub online: bool,
    pub routes: Vec<Route>,
    /// Every HTTP request so far, answered or not.
    pub requests: Vec<HttpRequest>,
}

/// One scripted cloud reply: for `method` when `url` is part of the request URL. It answers
/// every such request until a newer route for the same request is added.
pub struct Route {
    pub method: Method,
    pub url: String,
    pub status: u16,
    pub body: Vec<u8>,
}

impl Cloud {
    pub fn route(&mut self, method: Method, url: &str, status: u16, body: impl Into<Vec<u8>>) {
        self.routes.push(Route {
            method,
            url: url.into(),
            status,
            body: body.into(),
        });
    }

    /// Cloud preset `id` named `name`: its product record and its download, whose payload
    /// archive holds `preset`.
    pub fn preset(&mut self, id: &str, name: &str, preset: &Preset) {
        let product = format!(r#"{{"id":"{id}","name":"{name}","type":"preset"}}"#);
        self.route(Method::Get, &format!("/products/{id}"), 200, product);
        let archive = tetrad_services::backup::pack(&[("payload.pb", &preset.to_bytes())]).unwrap();
        let payload = format!(r#"{{"payload":"{}"}}"#, standard_base64(&archive));
        self.route(
            Method::Get,
            &format!("/products/{id}/download"),
            200,
            payload,
        );
    }

    /// The requests with `method` whose URL holds `url`.
    pub fn sent(&self, method: Method, url: &str) -> Vec<&HttpRequest> {
        (self.requests.iter())
            .filter(|request| request.method == method && request.url.contains(url))
            .collect()
    }
}

/// The NTP time of the online [`Cloud`] (2025-10-09T08:00:00Z).
pub const CLOUD_TIME: u32 = 1_759_996_800;

/// A `wpa_supplicant` event that associates the online sim with "Home".
pub const CONNECTED: &str =
    "<3>CTRL-EVENT-CONNECTED - Connection to 02:00:00:00:00:01 completed [id=1 id_str=]";

/// Offline: no supplicant. Online: the saved network "Home" and `OK` to every other request.
pub struct SimSupplicant(Rc<RefCell<Cloud>>);

impl SupplicantLink for SimSupplicant {
    fn request(&mut self, command: &str) -> Result<String, LinkError> {
        if !self.0.borrow().online {
            return Err(LinkError::Transport(
                "simulated platform has no supplicant".into(),
            ));
        }
        Ok(match command {
            "INTERFACES" => "wlan0\n",
            "LIST_NETWORKS" => "network id / ssid / bssid / flags\n1\tHome\tany\t[CURRENT]\n",
            _ if command.starts_with("BSS ") => "FAIL",
            _ => "OK",
        }
        .into())
    }
}

/// Offline: the scripted runner (no script: every command is refused). Online, the Wi-Fi
/// bring-up, DHCP and the NTP clock set succeed; other commands go to the script.
pub struct SimRunner {
    script: ScriptedRunner,
    cloud: Rc<RefCell<Cloud>>,
}

impl CommandRunner for SimRunner {
    type Child = FakeChild;

    fn run(&mut self, command: &Command) -> Result<Output, CommandError> {
        let line = command.line();
        let stdout = match line.as_str() {
            _ if !self.cloud.borrow().online => None,
            "modprobe wilc-spi" | "ifconfig wlan0 up" => Some(""),
            _ if line.starts_with("wpa_supplicant -B ") || line.starts_with("date -u -s ") => {
                Some("")
            }
            "udhcpc --now --quit -i wlan0" => {
                Some("udhcpc: Lease of 192.0.2.40 obtained, lease time 600\n")
            }
            "ifconfig wlan0" => {
                Some("          inet addr:192.0.2.40  Bcast:192.0.2.255  Mask:255.255.255.0\n")
            }
            "route -n" => Some(
                "0.0.0.0         192.0.2.1       0.0.0.0         UG    0      0        0 wlan0\n",
            ),
            _ => None,
        };
        match stdout {
            Some(stdout) => {
                self.script.calls.push(command.clone());
                Ok(Output::code(0).with_stdout(stdout))
            }
            None => self.script.run(command),
        }
    }

    fn run_cancellable(
        &mut self,
        command: &Command,
        cancel: &CancelToken,
    ) -> Result<Output, CommandError> {
        let online = self.cloud.borrow().online;
        match online {
            true => self.run(command),
            false => self.script.run_cancellable(command, cancel),
        }
    }

    fn spawn(&mut self, command: &Command) -> Result<FakeChild, CommandError> {
        self.script.spawn(command)
    }
}

/// Offline: the scripted network (no script: no answer). Online: every name is 192.0.2.1,
/// every TCP connect succeeds and every datagram gets an NTP reply at [`CLOUD_TIME`].
pub struct SimNet {
    script: ScriptedNet,
    cloud: Rc<RefCell<Cloud>>,
}

impl NetIo for SimNet {
    fn resolve(&mut self, host: &str) -> Result<Vec<IpAddr>, NetError> {
        match self.cloud.borrow().online {
            true => Ok(vec![IpAddr::V4(Ipv4Addr::new(192, 0, 2, 1))]),
            false => self.script.resolve(host),
        }
    }

    fn tcp_connect(&mut self, target: SocketAddr, timeout: Duration) -> Result<(), NetError> {
        match self.cloud.borrow().online {
            true => Ok(()),
            false => self.script.tcp_connect(target, timeout),
        }
    }

    fn udp_request(
        &mut self,
        target: SocketAddr,
        payload: &[u8],
        timeout: Duration,
    ) -> Result<Vec<u8>, NetError> {
        if !self.cloud.borrow().online {
            return self.script.udp_request(target, payload, timeout);
        }
        let mut reply = vec![0u8; 48];
        reply[0] = 0x24;
        reply[1] = 2;
        reply[40..44].copy_from_slice(&(CLOUD_TIME + 2_208_988_800).to_be_bytes());
        Ok(reply)
    }
}

/// Offline: the scripted HTTP client (no script: every request fails). Online: the
/// [`Cloud`] routes; a request without a route fails with its method and URL.
pub struct SimHttp {
    script: ScriptedHttp,
    cloud: Rc<RefCell<Cloud>>,
}

impl SimHttp {
    fn answer(&mut self, request: &HttpRequest) -> Option<Result<HttpResponse, HttpError>> {
        let mut cloud = self.cloud.borrow_mut();
        if !cloud.online {
            return None;
        }
        cloud.requests.push(request.clone());
        let route = (cloud.routes.iter().rev())
            .find(|route| route.method == request.method && request.url.contains(&route.url));
        Some(match route {
            Some(route) => Ok(HttpResponse {
                status: route.status,
                body: route.body.clone(),
            }),
            None => Err(HttpError::Transport(format!(
                "no simulated route for {} {}",
                request.method.as_str(),
                request.url
            ))),
        })
    }
}

impl HttpClient for SimHttp {
    fn send(&mut self, request: &HttpRequest) -> Result<HttpResponse, HttpError> {
        self.answer(request)
            .unwrap_or_else(|| self.script.send(request))
    }

    fn download(&mut self, request: &HttpRequest, dest: &Path) -> Result<u16, HttpError> {
        let Some(answer) = self.answer(request) else {
            return self.script.download(request, dest);
        };
        let response = answer?;
        (self.script.files)
            .write(dest, &response.body)
            .map_err(|error| HttpError::Transport(error.to_string()))?;
        Ok(response.status)
    }
}

pub struct Services {
    runner: SimRunner,
    files: MemFiles,
    net: SimNet,
    http: SimHttp,
    supplicant: SimSupplicant,
    worker: Rc<Cell<bool>>,
}

impl ServiceBackends for Services {
    type Runner = SimRunner;
    type Files = MemFiles;
    type Net = SimNet;
    type Http = SimHttp;
    type Supplicant = SimSupplicant;
    fn runner(&mut self) -> &mut SimRunner {
        &mut self.runner
    }
    fn files(&mut self) -> &mut MemFiles {
        &mut self.files
    }
    fn net(&mut self) -> &mut SimNet {
        &mut self.net
    }
    fn http(&mut self) -> &mut SimHttp {
        &mut self.http
    }
    fn supplicant(&mut self) -> &mut SimSupplicant {
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
    /// Inline by default; with [`Sim::worker`] set a job stays running until the test queues
    /// its notice ([`Sim::inputs`]), as a platform worker answers later.
    fn run_job(&mut self, job: ServiceJob) -> Option<ServiceNotice> {
        (!self.worker.get()).then(|| tetrad_app::device::services::run_job(self, job))
    }
}

/// SHARC memory for Neural Capture V1 transfers: one sparse byte map by physical address.
#[derive(Default)]
struct SimSharcMemory(BTreeMap<u32, u8>);

impl SharcMemory for SimSharcMemory {
    fn write(&mut self, address: u32, bytes: &[u8]) -> std::io::Result<()> {
        for (at, &byte) in (address..).zip(bytes) {
            self.0.insert(at, byte);
        }
        Ok(())
    }

    fn read(&mut self, address: u32, out: &mut [u8]) -> std::io::Result<()> {
        for (at, byte) in (address..).zip(out) {
            *byte = self.0.get(&at).copied().unwrap_or(0);
        }
        Ok(())
    }
}

pub struct Sim {
    pub clock: SimClock,
    hal: SimHal,
    settings: Settings,
    recorder: Recorder,
    services: Services,
    fixture: Fixture,
    media: PathBuf,
    pub markers: Vec<Marker>,
    pub firmware: SharcFirmware,
    /// The loader bus, once the boot opened it.
    pub bus: Option<Bus>,
    /// Core 0 and core 1 model windows.
    pub core_windows: [Window; 2],
    /// Platform inputs a test queues (for example [`Input::Service`]); the next poll takes
    /// them.
    pub inputs: Rc<RefCell<Vec<Input>>>,
    /// Service jobs run on a simulated worker: no inline notice ([`Services::run_job`]).
    pub worker: Rc<Cell<bool>>,
    /// The wall clock; `None` (default) refuses preset saves.
    pub wall_clock: Cell<Option<SystemTime>>,
    /// The device media cipher; `None` (default) leaves the IR library out. With a cipher the
    /// platform also serves an empty factory IR catalog.
    pub media_cipher: Option<tetrad_media::factory::DeviceCipher>,
    /// The last value of the random source (a fixed step sequence, so runs repeat).
    random: u32,
    /// The network and cloud; offline by default.
    pub cloud: Rc<RefCell<Cloud>>,
    /// [`Platform::drill_core0_silence`] fires once (`--drill-core0`).
    pub drill_core0: bool,
    /// [`Platform::disk_space`]; `None` (default) is no reading.
    pub disk_space: Option<tetrad_sys::fs::FsSpace>,
    /// Extension runners and store worker; `None` (default): extensions do not run.
    pub extensions: Option<tetrad_app::device::extensions::LocalBackend>,
    /// The Remote server; `None` (default): Remote does not run.
    pub remote: Option<FakeRemote>,
}

/// A Remote server with no socket: tests push [`Input::Remote`] calls and read the replies.
#[derive(Debug, Default)]
pub struct FakeRemote {
    pub running: bool,
    /// `start` fails with this reason (a port in use).
    pub fail: Option<String>,
    pub replies: Vec<(u64, tetrad_remote::Reply)>,
    /// PINs made so far; the next PIN is this count plus one, as four digits.
    pub pins: u32,
}

impl tetrad_app::device::remote::RemoteBackend for FakeRemote {
    fn start(&mut self) -> Result<(), String> {
        if let Some(reason) = &self.fail {
            return Err(reason.clone());
        }
        self.running = true;
        Ok(())
    }
    fn stop(&mut self) {
        self.running = false;
    }
    fn reply(&mut self, id: u64, reply: tetrad_remote::Reply) {
        self.replies.push((id, reply));
    }
    fn new_pin(&mut self) -> Result<String, String> {
        self.pins += 1;
        Ok(format!("{:04}", self.pins))
    }
}

impl Sim {
    /// The service commands the device ran so far (scripted or not).
    pub fn commands(&self) -> &[Command] {
        &self.services.runner.script.calls
    }

    /// Saves a signed-in account ("player") with made-up tokens that never expire, as
    /// `AccountConfig.ini` holds them after a sign-in. Call before `Device::start`.
    pub fn sign_in(&mut self) {
        use tetrad_settings::Secret;
        use tetrad_settings::account::AccountTokens;
        use tetrad_settings::device::{DeviceSettings, Hardware};
        let (mut settings, _) = DeviceSettings::load(&self.settings, Hardware::QuadCortex).unwrap();
        settings.update(|s| {
            s.tokens = AccountTokens {
                access_token: Secret::new(jwt(r#"{"sub":"user-1","exp":4102444800}"#)),
                refresh_token: Secret::new("refresh-synthetic"),
            };
            s.account.username = "player".into();
        });
        settings.flush(&mut self.settings).unwrap();
    }

    /// The simulated platform serving `fixture` over `media`, logging to `log`.
    pub fn new(fixture: Fixture, media: &Media, log: &Rc<RefCell<Log>>) -> Self {
        let clock = SimClock {
            now: Rc::default(),
            rings: Window::new(MESSAGE_MEMORY_BYTES),
            log: log.clone(),
            hold: Rc::default(),
            hold_until: Rc::default(),
        };
        let worker = Rc::<Cell<bool>>::default();
        let cloud = Rc::<RefCell<Cloud>>::default();
        let files = MemFiles::default();
        Self {
            clock: clock.clone(),
            hal: SimHal {
                hal: RecordingHal::default(),
                clock: clock.clone(),
            },
            settings: Settings {
                files: BTreeMap::new(),
                log: log.clone(),
            },
            recorder: Recorder(clock),
            services: Services {
                runner: SimRunner {
                    script: ScriptedRunner::default(),
                    cloud: cloud.clone(),
                },
                files: files.clone(),
                net: SimNet {
                    script: ScriptedNet::default(),
                    cloud: cloud.clone(),
                },
                http: SimHttp {
                    script: ScriptedHttp::sharing(files),
                    cloud: cloud.clone(),
                },
                supplicant: SimSupplicant(cloud.clone()),
                worker: worker.clone(),
            },
            cloud,
            fixture,
            media: media.0.clone(),
            markers: Vec::new(),
            firmware: SharcFirmware {
                loadsharc: loadsharc(),
                core1: kernel(0xac, 3000),
                core2: kernel(0xab, 40),
                soc2: Soc2Firmware {
                    normal: Soc2Pair {
                        spl: vec![0x5a; 40],
                        core0: vec![0xc0; 24],
                    },
                    factory: None,
                },
            },
            bus: None,
            core_windows: [Window::new(0x1000), Window::new(0x1000)],
            inputs: Rc::default(),
            worker,
            wall_clock: Cell::default(),
            media_cipher: None,
            random: 0,
            drill_core0: false,
            disk_space: None,
            extensions: None,
            remote: None,
        }
    }

    /// Queues a HAL event (a jack detect, a stomp) for the next HAL poll.
    pub fn inject_hal(&mut self, event: HalEvent) {
        self.hal.hal.inject(event);
    }
}

impl Platform for Sim {
    type Error = std::io::Error;
    type Clock = SimClock;
    type BootBus = Bus;
    type Window = Window;
    type RingDrain = MainLoop;
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
    fn marker(&mut self, marker: Marker) -> std::io::Result<bool> {
        Ok(self.markers.contains(&marker))
    }
    fn set_marker(&mut self, marker: Marker) -> std::io::Result<()> {
        self.markers.push(marker);
        Ok(())
    }
    fn boot_count(&mut self) -> std::io::Result<u32> {
        Ok(1)
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
    fn firmware(&mut self, variant: Variant) -> std::io::Result<SharcFirmware> {
        assert_eq!(variant, Variant::QuadCortex);
        Ok(self.firmware.clone())
    }
    fn boot_bus(&mut self) -> std::io::Result<Bus> {
        let booter = StockBooter::parse(&self.firmware.loadsharc).unwrap();
        let peers = vec![
            SimSharc::new(&booter.core2, 0x0024_0100),
            SimSharc::new(&booter.core1, 0x200b_0000),
        ];
        let bus = Bus(Rc::new(RefCell::new(RecordingBus::new(peers))));
        self.bus = Some(bus.clone());
        Ok(bus)
    }
    fn map_soc2_buffer(&mut self, _: &mut Bus, buffer: PhysAddr, _: u32) -> std::io::Result<()> {
        assert_eq!(buffer, PhysAddr(SOC2_BUFFER));
        Ok(())
    }
    fn open_soc2(&mut self) -> std::io::Result<bool> {
        Ok(true)
    }
    fn drill_core0_silence(&mut self) -> bool {
        std::mem::take(&mut self.drill_core0)
    }
    fn dsp_windows(&mut self) -> std::io::Result<DspWindows<Window>> {
        let window = |core, memory| ModelMemory::new(core, Address::new(CORE_WINDOW_BASE), memory);
        Ok(DspWindows {
            messages: self.clock.rings.clone(),
            cores: CoreWindows::new(
                window(DspCore::Core0, self.core_windows[0].clone()).unwrap(),
                window(DspCore::Core1, self.core_windows[1].clone()).unwrap(),
            )
            .unwrap(),
        })
    }
    fn dsp_file(&mut self, file: DspFile<'_>) -> std::io::Result<Vec<u8>> {
        Ok(match file {
            DspFile::ModelRepo => crypt(self.fixture.models.as_bytes(), &REPO_KEY),
            DspFile::ModuleReport => self.fixture.report.as_bytes().to_vec(),
            DspFile::Blob(name) => crypt(&(self.fixture.blob)(name).to_bytes().unwrap(), &BLOB_KEY),
            DspFile::FactoryIrCatalog if self.media_cipher.is_some() => {
                tetrad_media::catalog::FactoryIrCatalog::new(Vec::new()).encode()
            }
            // No stock image or IR library here: table drivers stay muted, cabs load the
            // default IR.
            DspFile::ZenUi | DspFile::FactoryIrCatalog | DspFile::FactoryIr(_) => {
                return Err(std::io::ErrorKind::NotFound.into());
            }
            DspFile::Diffs(_) => DiffFile {
                records: self
                    .fixture
                    .diffs
                    .iter()
                    .map(|&name| DiffRecord {
                        name: name.into(),
                        contents: Vec::new(),
                        relocations: Vec::new(),
                        ranges: Vec::new(),
                    })
                    .collect(),
            }
            .to_bytes()
            .unwrap(),
        })
    }
    fn wall_clock(&mut self) -> Option<SystemTime> {
        self.wall_clock.get()
    }
    fn disk_space(&mut self) -> Option<tetrad_sys::fs::FsSpace> {
        self.disk_space
    }
    fn media_cipher(&mut self) -> Option<tetrad_media::factory::DeviceCipher> {
        self.media_cipher.clone()
    }
    fn poll_input(&mut self, out: &mut Vec<Input>) -> std::io::Result<()> {
        out.append(&mut self.inputs.borrow_mut());
        if let Some(extensions) = &mut self.extensions {
            extensions.drain(out);
        }
        Ok(())
    }
    fn extensions(&mut self) -> Option<&mut dyn tetrad_app::device::extensions::ExtensionBackend> {
        self.extensions.as_mut().map(|backend| backend as _)
    }
    fn remote(&mut self) -> Option<&mut dyn tetrad_app::device::remote::RemoteBackend> {
        self.remote.as_mut().map(|backend| backend as _)
    }
    fn midi_write(&mut self, bytes: &[u8]) -> std::io::Result<()> {
        self.clock.log.borrow_mut().midi.push(bytes.to_vec());
        Ok(())
    }
    fn present(&mut self, rgb: &[u8]) -> std::io::Result<()> {
        self.clock.log.borrow_mut().frames.push(rgb.len());
        Ok(())
    }
    fn forget_display(&mut self) {
        self.clock.log.borrow_mut().display_forgets += 1;
    }
    fn log(&mut self, notice: &Notice) {
        self.clock.log.borrow_mut().notices.push(notice.clone());
    }
    fn sharc_memory(&mut self) -> Option<Box<dyn SharcMemory>> {
        Some(Box::new(SimSharcMemory::default()))
    }
    fn random_u32(&mut self) -> Option<u32> {
        self.random = self.random.wrapping_add(0x9e37_79b9);
        Some(self.random)
    }
}

/// One code section of [`CODE`], entry at its first word.
pub fn light_blob() -> ModuleBlob {
    ModuleBlob {
        contents: vec![SectionBytes {
            section: Section::Code,
            bytes: CODE.to_vec(),
        }],
        relocations: Vec::new(),
        ranges: vec![LinkRange::for_contents(Section::Code, CODE.len()).unwrap()],
        entry: Section::Code.base() + 1,
    }
}

/// Invented loadsharc-shaped ELF: one PT_LOAD with both booter tables (name section and
/// booter code per core) at the stock config addresses.
fn loadsharc() -> Vec<u8> {
    const SEGMENT_VA: u32 = 0x12000;
    const OFFSET: u32 = 0x1000;
    const FILE_END: u32 = 0x17cc0;
    const MEMORY_END: u32 = 0x17d10;
    let mut elf = vec![0; (OFFSET + FILE_END - SEGMENT_VA) as usize];
    let mut put = |at: u32, bytes: &[u8]| {
        let at = at as usize;
        elf[at..at + bytes.len()].copy_from_slice(bytes);
    };
    put(0, b"\x7fELF\x01\x01\x01\x00");
    put(0x10, &2_u16.to_le_bytes());
    put(0x12, &40_u16.to_le_bytes());
    put(0x1c, &0x34_u32.to_le_bytes());
    put(0x2a, &32_u16.to_le_bytes());
    put(0x2c, &1_u16.to_le_bytes());
    let header = [
        1,
        OFFSET,
        SEGMENT_VA,
        SEGMENT_VA,
        FILE_END - SEGMENT_VA,
        MEMORY_END - SEGMENT_VA,
    ];
    for (index, value) in header.into_iter().enumerate() {
        put(0x34 + 4 * index as u32, &value.to_le_bytes());
    }
    let va = |va: u32| OFFSET + va - SEGMENT_VA;
    put(va(0x13300), &[0xa1, 0xa2, 0xa3, 0xa4, 0xa5, 0xa6]);
    for (config, mailbox, app_start, name_va, name) in [
        (
            0x123c0,
            0x200b_c330,
            0x00bb_d810,
            0x13000,
            b"SharcBooter_Core1.dxe\0",
        ),
        (
            0x15040,
            0x200a_e330,
            0x00bb_6810,
            0x13100,
            b"SharcBooter_Core2.dxe\0",
        ),
    ] {
        put(va(name_va), name);
        let table = [
            mailbox,
            mailbox + 0xa74,
            mailbox + 0xa70,
            mailbox + 0xa6c,
            mailbox + 0xa68,
            app_start,
            2,
            0x200b_b008,
            name_va,
            22,
            app_start,
            0x13300,
            6,
        ];
        let bytes: Vec<u8> = table.into_iter().flat_map(u32::to_le_bytes).collect();
        put(va(config), &bytes);
    }
    elf
}

/// Raw LDR: FIRST|IGNORE, one literal of `payload` bytes, one fill, FINAL.
fn kernel(signature: u8, payload: usize) -> Vec<u8> {
    let header = |flags: u16, target: u32, count: u32, argument: u32| {
        let mut raw = [0_u8; 16];
        raw[..2].copy_from_slice(&flags.to_le_bytes());
        raw[3] = signature;
        raw[4..8].copy_from_slice(&target.to_le_bytes());
        raw[8..12].copy_from_slice(&count.to_le_bytes());
        raw[12..].copy_from_slice(&argument.to_le_bytes());
        raw[2] = raw.iter().fold(0, |xor, byte| xor ^ byte);
        raw
    };
    let final_offset = (48 + payload) as u32;
    let mut bytes = header(0x5001, 0x0016_1894, 0, final_offset).to_vec();
    bytes.extend(header(0x0001, 0x2824_0000, payload as u32, 0));
    bytes.extend((0..payload).map(|index| index as u8));
    bytes.extend(header(0x0101, 0x2825_0000, 0x40, 0));
    bytes.extend(header(0x8001, 0x0016_1894, 0, 0));
    bytes
}

/// A [`STOCK`] preset with one `Light` block (GAIN 0.5) in row 1 at `column`.
pub fn light_preset(specs: &Specs, column: usize) -> Preset {
    block_preset(
        specs,
        Model::block(1, [ParamValue::Float(0.5), ParamValue::Float(0.0)]),
        column,
    )
}

/// A [`STOCK`] preset with `block` in row 1 at `column`.
pub fn block_preset(specs: &Specs, block: Model, column: usize) -> Preset {
    let catalog = SpecsCatalog(specs);
    let mut preset = Preset::new_default("4.1.0", &catalog).unwrap();
    preset
        .insert_block(Slot { row: 0, column }, block, &catalog)
        .unwrap();
    preset
}

/// A temp `/media/p4`: one setlist with the given presets from slot 0, a stock-shaped
/// `zenui.db` and the factory `factory.db`. Removed on drop.
pub struct Media(pub PathBuf);

/// The device path of the [`Media`] setlist folder.
pub const SETLIST: &str = "/media/p4/Presets/My Presets";

impl Media {
    /// `presets` are `(name, preset)`; each is stored as `<name>.pb`.
    pub fn new<'a>(presets: impl IntoIterator<Item = (&'a str, Preset)>) -> Self {
        // Tests run in parallel: one directory per call.
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let root = std::env::temp_dir().join(format!(
            "tetrad-app-sim-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let setlist = root.join("Presets/My Presets");
        fs::create_dir_all(&setlist).unwrap();
        fs::create_dir_all(root.join("factory")).unwrap();
        let mut slots = vec![None; SETLIST_SLOTS];
        for (slot, (name, preset)) in slots.iter_mut().zip(presets) {
            fs::write(setlist.join(format!("{name}.pb")), preset.to_bytes()).unwrap();
            *slot = Some(format!("{SETLIST}/{name}.pb"));
        }
        fs::write(setlist.join("setlist"), encode_setlist(&slots).unwrap()).unwrap();
        fs::write(root.join("user_preset_folders"), format!("{SETLIST}\n")).unwrap();
        for (db, ddl) in [("zenui.db", DDL), ("factory/factory.db", FACTORY_DDL)] {
            rusqlite::Connection::open(root.join(db))
                .unwrap()
                .execute_batch(ddl)
                .unwrap();
        }
        Self(root)
    }
}

impl Drop for Media {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Boots the simulated [`STOCK`] device over `media` to its first deployed preset. As on a
/// CorOS 4.1.0 rootfs, the capture corruption check and conversion markers exist, so the boot
/// runs no capture step (`0x704198`). A test of those steps builds its own [`Sim`].
pub fn boot(media: Media) -> (Device<Sim>, Rc<RefCell<Log>>, Media) {
    boot_with(media, tetrad_app::device::bringup::RunOptions::STOCK)
}

/// [`boot`] under start-up `options` (for example a `--preset` file).
pub fn boot_with(
    media: Media,
    options: tetrad_app::device::bringup::RunOptions,
) -> (Device<Sim>, Rc<RefCell<Log>>, Media) {
    use tetrad_app::device::media::{CAPTURE_CONVERSION_FINISHED, CAPTURE_CORRUPTION_CHECKED};
    let log = Rc::new(RefCell::new(Log::default()));
    let sim = Sim::new(STOCK, &media, &log);
    for marker in [CAPTURE_CORRUPTION_CHECKED, CAPTURE_CONVERSION_FINISHED] {
        sim.services.files.put(marker, "");
    }
    let device = Device::start_with(sim, options).unwrap();
    (device, log, media)
}

/// [`boot`] over the service files `files` (kept across restarts by the caller: clones share
/// their contents).
#[allow(dead_code)]
pub fn boot_with_files(media: Media, files: MemFiles) -> (Device<Sim>, Rc<RefCell<Log>>, Media) {
    use tetrad_app::device::media::{CAPTURE_CONVERSION_FINISHED, CAPTURE_CORRUPTION_CHECKED};
    let log = Rc::new(RefCell::new(Log::default()));
    let mut sim = Sim::new(STOCK, &media, &log);
    sim.services.files = files;
    for marker in [CAPTURE_CORRUPTION_CHECKED, CAPTURE_CONVERSION_FINISHED] {
        sim.services.files.put(marker, "");
    }
    let device = Device::start_with(sim, tetrad_app::device::bringup::RunOptions::STOCK).unwrap();
    (device, log, media)
}

/// [`boot`] with `remote`, the media cipher and a wall clock (an IR import needs both), over
/// the service files `files`.
#[allow(dead_code)]
pub fn boot_remote(media: Media, files: MemFiles, remote: FakeRemote) -> (Device<Sim>, Media) {
    use tetrad_app::device::media::{CAPTURE_CONVERSION_FINISHED, CAPTURE_CORRUPTION_CHECKED};
    let log = Rc::new(RefCell::new(Log::default()));
    let mut sim = Sim::new(STOCK, &media, &log);
    sim.services.files = files;
    for marker in [CAPTURE_CORRUPTION_CHECKED, CAPTURE_CONVERSION_FINISHED] {
        sim.services.files.put(marker, "");
    }
    sim.media_cipher = Some(tetrad_media::factory::DeviceCipher::caller_secret(
        b"invented",
    ));
    sim.wall_clock.set(Some(
        std::time::UNIX_EPOCH + Duration::from_secs(1_791_635_696),
    ));
    sim.remote = Some(remote);
    let device = Device::start_with(sim, tetrad_app::device::bringup::RunOptions::STOCK).unwrap();
    (device, media)
}

/// [`boot`] with extension runners on threads and the store under the media root, over the
/// service files `files` (kept across restarts by the caller: clones share their contents).
#[allow(dead_code)]
pub fn boot_extensions(
    media: Media,
    backend: tetrad_app::device::extensions::LocalBackend,
    files: MemFiles,
) -> (Device<Sim>, Rc<RefCell<Log>>, Media) {
    use tetrad_app::device::media::{CAPTURE_CONVERSION_FINISHED, CAPTURE_CORRUPTION_CHECKED};
    let log = Rc::new(RefCell::new(Log::default()));
    let mut sim = Sim::new(STOCK, &media, &log);
    sim.services.files = files;
    for marker in [CAPTURE_CORRUPTION_CHECKED, CAPTURE_CONVERSION_FINISHED] {
        sim.services.files.put(marker, "");
    }
    sim.extensions = Some(backend);
    let device = Device::start_with(sim, tetrad_app::device::bringup::RunOptions::STOCK).unwrap();
    (device, log, media)
}

/// [`boot`] with the [`Cloud`] online and the account signed in ([`Sim::sign_in`]), then the
/// Wi-Fi association: after DHCP, the internet probe and NTP the device is online. The
/// cloud routes are empty; tests add them through `device.platform().cloud`.
pub fn boot_online(media: Media) -> (Device<Sim>, Media) {
    use tetrad_app::device::media::{CAPTURE_CONVERSION_FINISHED, CAPTURE_CORRUPTION_CHECKED};
    let log = Rc::new(RefCell::new(Log::default()));
    let sim = Sim::new(STOCK, &media, &log);
    for marker in [CAPTURE_CORRUPTION_CHECKED, CAPTURE_CONVERSION_FINISHED] {
        sim.services.files.put(marker, "");
    }
    (start_online(sim), media)
}

/// Starts `sim` with its [`Cloud`] online and the account signed in, then associates Wi-Fi
/// ([`go_online`]).
pub fn start_online(mut sim: Sim) -> Device<Sim> {
    sim.cloud.borrow_mut().online = true;
    sim.sign_in();
    let mut device = Device::start(sim).unwrap();
    go_online(&mut device);
    device
}

/// Associates the online sim ([`CONNECTED`]) and steps until the device reports internet.
pub fn go_online(device: &mut Device<Sim>) {
    let inputs = device.platform().inputs.clone();
    inputs
        .borrow_mut()
        .push(Input::Service(ServiceNotice::Supplicant(CONNECTED.into())));
    for _ in 0..20 {
        device.step();
        if device.bridges().ui.view().snapshot().cloud_ready() {
            return;
        }
    }
    let wifi = &device.bridges().ui.view().snapshot().wifi;
    panic!("the sim is not online: {:?}", wifi.status);
}

/// Standard padded base64, as the cloud sends a product payload.
pub fn standard_base64(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let n = chunk.iter().fold(0u32, |n, &b| (n << 8) | u32::from(b)) << (8 * (3 - chunk.len()));
        for i in 0..4 {
            out.push(match i <= chunk.len() {
                true => TABLE[(n >> (18 - 6 * i) & 63) as usize] as char,
                false => '=',
            });
        }
    }
    out
}

/// A made-up JWT: header `{}`, the `claims`, signature "sig" (base64url, no padding).
pub fn jwt(claims: &str) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::new();
    for chunk in claims.as_bytes().chunks(3) {
        let n = chunk.iter().fold(0u32, |n, &b| (n << 8) | u32::from(b)) << (8 * (3 - chunk.len()));
        for i in 0..=chunk.len() {
            out.push(TABLE[(n >> (18 - 6 * i) & 63) as usize] as char);
        }
    }
    format!("e30.{out}.sig")
}

/// One Cortex Control host message in as HID reports, then idle steps so the scripted DSP
/// reads the device's reports.
pub fn host_send<P: Platform>(device: &mut Device<P>, message: impl Into<ControlMessage>) {
    let message = message.into();
    let envelope = Envelope {
        message_type: message.message_type() as u32,
        transform: 0,
        decompress: 0,
        opaque: [0; 2],
        payload: message.encode(LinkLimits::default().payload).unwrap(),
    };
    for report in envelope.reports(Limits::default()).unwrap() {
        device.push(Event::Control(Box::new(report)));
        device.step();
    }
    (0..50).for_each(|_| _ = device.step());
}

/// The stock Cortex Control handshake: KeepAlive UPDATE, ModelRepo READ, Connection.
pub fn connect_control<P: Platform>(device: &mut Device<P>) {
    host_send(
        device,
        KeepAliveMessage {
            action: Some(MessageAction::Update.into()),
            ..Default::default()
        },
    );
    host_send(
        device,
        ModelRepoMessage {
            action: Some(MessageAction::Read.into()),
            ..Default::default()
        },
    );
    host_send(
        device,
        ConnectionMessage {
            connected: Some(true),
            ..Default::default()
        },
    );
    assert!(device.bridges().control.connected());
}

/// A KeepAlive UPDATE with `is_online` (`0x6e0740`): with Control connected, `online` makes it
/// the route of the cloud calls and the cloud usable without Wi-Fi (re-r44 §3). The route also
/// needs the link transform (review-r45 M4), so going online keys a link that has none
/// ([`key_control`]).
pub fn set_control_online(device: &mut Device<Sim>, online: bool) {
    host_send(
        device,
        KeepAliveMessage {
            action: Some(MessageAction::Update.into()),
            is_online: Some(online),
            ..Default::default()
        },
    );
    if online && device.bridges().control.cloud_route().is_none() {
        key_control(device);
    }
}

/// The Version READ key exchange with fake vectors: the device sends the DSP key request
/// `0x42`, the scripted DSP answers `0x17`, and the link has its transform
/// (`CommsCipher::derive(CONTROL_KEY)`).
pub fn key_control(device: &mut Device<Sim>) {
    host_send(
        device,
        VersionMessage {
            action: Some(MessageAction::Read.into()),
            ..Default::default()
        },
    );
    let answer = encode(
        ResponseType::UsbMeterInput,
        0,
        &[[7; 16], *CONTROL_KEY].concat(),
    );
    let rings = &device.platform().clock.rings;
    publish(
        &mut rings.0.borrow_mut(),
        CoreId::Dsp(DspCore::Core3),
        &answer,
    );
    (0..50).for_each(|_| _ = device.step());
}

/// The fake transform vector of [`key_control`].
pub const CONTROL_KEY: &[u8; 16] = b"fake vector B 16";

/// The uncompressed Control messages the device sent from request `mark` on: `0x27` frames
/// with byte 2 = 0, report at `frame[16..144]`. Scene (13) pushes are checked with their own
/// codec and left out: `ControlMessage` keeps Scene outside on purpose.
pub fn control_sent(log: &Log, mark: usize) -> Vec<ControlMessage> {
    let mut reassembler = Reassembler::new(Limits::default());
    (log.requests[mark..].iter())
        .filter(|sent| sent.kind() == 0x27 && sent.frame[2] == 0)
        .filter_map(|sent| {
            reassembler
                .push(&sent.frame[HEADER_BYTES..HEADER_BYTES + REPORT_BYTES])
                .unwrap()
        })
        .filter(|envelope| envelope.decompress == 0)
        .filter(|envelope| {
            let scene = envelope.message_type == CortexMessageType::Scene as u32;
            if scene {
                let payload = envelope.plain_payload().unwrap();
                SceneMessage::decode(payload, LinkLimits::default().payload).unwrap();
            }
            !scene
        })
        .map(|envelope| {
            ControlMessage::decode(&envelope, LinkLimits::default().payload)
                .unwrap()
                .message
        })
        .collect()
}

/// Center of the enabled hit region for `control`, pressed.
pub fn target(ui: &Ui, control: Control) -> TouchSample {
    let region = ui
        .view()
        .hit_regions()
        .into_iter()
        .find(|region| region.enabled && region.control == control)
        .unwrap_or_else(|| panic!("{control:?} is not on screen"));
    let (x, y) = region.bounds.center();
    TouchSample {
        x: x as u16,
        y: y as u16,
        pressed: true,
    }
}

/// Taps UPDATE on Settings → Updates, then INSTALL on the question that follows: a test
/// download has no offered version, so the Tetrad version guard asks first. `steps` runs after
/// each touch.
pub fn tap_update_install(device: &mut Device<Sim>, steps: impl Fn(&mut Device<Sim>)) {
    use tetrad_app::device::ui::UiInput;
    for control in [
        Control::Updater(tetrad_ui::UpdaterCommand::Install),
        Control::Dialog(tetrad_ui::DialogButton::Primary),
    ] {
        let tap = target(&device.bridges().ui, control);
        for pressed in [true, false] {
            device.push(Event::UiInput(UiInput::Touch(TouchSample {
                pressed,
                ..tap
            })));
            steps(device);
        }
    }
}

/// A synthetic V1 capture (`captureProto.Capture`, the converter input of `0x6ec290`) that
/// `tetrad_capture::stored::v1_runtime_data` accepts: 48 kHz, 512/2048 coefficients, one
/// 1→1 layer. Store it encrypted to make a capture row the boot check judges sound.
pub fn sound_v1_capture() -> Vec<u8> {
    fn key(out: &mut Vec<u8>, field: u32, wire: u32) {
        varint(out, field << 3 | wire);
    }
    fn varint(out: &mut Vec<u8>, mut value: u32) {
        while value >= 0x80 {
            out.push(value as u8 | 0x80);
            value >>= 7;
        }
        out.push(value as u8);
    }
    fn fixed(out: &mut Vec<u8>, field: u32, value: f32) {
        key(out, field, 5);
        out.extend(value.to_le_bytes());
    }
    fn bytes(out: &mut Vec<u8>, field: u32, payload: &[u8]) {
        key(out, field, 2);
        varint(out, payload.len() as u32);
        out.extend(payload);
    }
    let packed = |values: &[f32]| {
        values
            .iter()
            .flat_map(|v| v.to_le_bytes())
            .collect::<Vec<_>>()
    };
    let mut out = Vec::new();
    fixed(&mut out, 1, 48_000.0);
    key(&mut out, 2, 0);
    varint(&mut out, 512);
    key(&mut out, 3, 0);
    varint(&mut out, 2048);
    bytes(&mut out, 4, &packed(&[0.0; 512]));
    bytes(&mut out, 5, &packed(&[0.0; 2048]));
    fixed(&mut out, 6, 1.0);
    fixed(&mut out, 7, 1.0);
    key(&mut out, 8, 0);
    varint(&mut out, 1);
    bytes(&mut out, 9, &[1, 1]);
    let mut layer = Vec::new();
    bytes(&mut layer, 1, &packed(&[1.0]));
    bytes(&mut layer, 2, &packed(&[0.0]));
    bytes(&mut out, 10, &layer);
    fixed(&mut out, 11, 20.0);
    fixed(&mut out, 12, 0.7);
    out
}

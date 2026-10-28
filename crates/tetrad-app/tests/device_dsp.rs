//! The dsp and preset bridges on the shared simulated platform (`common`): a synthetic preset
//! deploys with the stock per-model setup, then a live parameter edit, a bypass toggle, a
//! scene recall and a save round trip.

mod common;

use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, SystemTime};

use common::{Fixture, Log, Media, Sim, light_blob};
use qc_format::blob::{LinkRange, ModuleBlob, Section, SectionBytes};
use tetrad_app::device::capture::{self, CapturePreset};
use tetrad_app::device::dsp::{self, Context, Dsp, DspError};
use tetrad_app::device::jacks::{InputChange, InputScheduler};
use tetrad_app::device::platform::{DspFile, Platform};
use tetrad_app::device::preset::{self, SaveRequest, SpecsCatalog};
use tetrad_app::device::services::Licences;
use tetrad_app::device::{Current, DspCommand, Effect, PresetRef};
use tetrad_capture::entry::Hardware;
use tetrad_catalog::{Catalog, Specs};
use tetrad_dsp_host::session::{Clock, SessionError};
use tetrad_dsp_host::setup::{DriverMidi, LinkedEdit};
use tetrad_dsp_protocol::requests::Request;
use tetrad_dsp_protocol::{CoreId, DspCore};
use tetrad_engine::{
    Column, Deployment, Endpoint, ModelId, Node, Placement, Row, SceneIndex, Site, Slot, Update,
};
use tetrad_hal::analog::GainEndpoint;
use tetrad_hal::hal::RecordingHal;
use tetrad_hal::platform::Platform as HalPlatform;
use tetrad_hal::zenjack::GainPlan;
use tetrad_library::sidecar::Creator;
use tetrad_library::stock::{StockLibrary, StockLimits};
use tetrad_loader::plan::Variant;
use tetrad_preset::{Model, ParamValue, Preset};
use tetrad_storage::media::MediaRoot;

const BYPASSED: u32 = 0x3f80_0000;
const MUTED_BYPASS: u32 = 0x4000_0000;

/// A drive (category 0, recalled first when bypassed; Grid plugin 1), a delay, a mono cab
/// (category 12: the simulation has no ZenUI image, so the cab driver's tables are missing and
/// the block must be muted), a compressor with a sidechain source (parameter 1) and level (`sidechain_in`,
/// parameter 2), a PCOMSkews blend (16008) whose driver converts its SOURCE (parameter 6),
/// and a drive whose blob does not fit a core.
const MODELS: &str = "<Models>\
<Category id='0' name='Drive'><Model id='1' name='Light' blob='light' plugin_id='1'>\
<Parameter name='GAIN' type='float' min='0' max='10' defaultValue='5'/></Model>\
<Model id='3' name='Huge' blob='huge'>\
<Parameter name='GAIN' type='float' min='0' max='10' defaultValue='5'/></Model></Category>\
<Category id='6' name='Delay'><Model id='2' name='Echo' blob='echo'>\
<Parameter name='TIME' type='float' defaultValue='0.3'/>\
<Parameter name='MIX' type='float' defaultValue='0.5'/></Model></Category>\
<Category id='5' name='Compressor'><Model id='5018' name='Comp' blob='comp' sidechain='1' \
sidechain_in='2'><Parameter name='THRESHOLD' type='float' min='-48' max='0' defaultValue='-40'/>\
<Parameter name='SOURCE' type='rotarySwitch' min='0' max='44' defaultValue='0' dynamic='true' \
stepNames='Off'/><Parameter name='SIDECHAIN IN' type='float' defaultValue='0.5'/></Model>\
</Category>\
<Category id='16' name='Pitch'><Model id='16008' name='Blend' blob='blend' sidechain='6'>\
<Parameter name='P0' type='float' defaultValue='0.5'/><Parameter name='P1' type='float' defaultValue='0.5'/>\
<Parameter name='P2' type='float' defaultValue='0.5'/><Parameter name='P3' type='float' defaultValue='0.5'/>\
<Parameter name='P4' type='float' defaultValue='0.5'/><Parameter name='P5' type='float' defaultValue='0.5'/>\
<Parameter name='SOURCE' type='rotarySwitch' min='0' max='44' defaultValue='0' dynamic='true' \
stepNames='Off'/></Model></Category>\
<Category id='12' name='Cab'><Model id='12000' name='Cab' blob='cab'>\
<Parameter name='LEVEL' type='float' defaultValue='0.5'/></Model></Category>\
<Category id='26' name='Utility'><Model id='26001' name='Freeze' blob='freeze'>\
<Parameter name='ON / OFF' type='toggleButton' min='0' max='1' defaultValue='0'/>\
<Parameter name='MOMENTARY' type='switch' min='0' max='1' defaultValue='0'/></Model></Category>\
<Category id='18' name='Synth'><Model id='18007' name='Minivoicer' blob='minivoicer'>\
<Parameter name='P0' type='float' defaultValue='0'/><Parameter name='P1' type='float' defaultValue='0'/><Parameter name='P2' type='float' defaultValue='0'/><Parameter name='P3' type='float' defaultValue='0'/>\
<Parameter name='P4' type='float' defaultValue='0'/><Parameter name='P5' type='float' defaultValue='0'/><Parameter name='P6' type='float' defaultValue='0'/><Parameter name='P7' type='float' defaultValue='0'/>\
<Parameter name='P8' type='float' defaultValue='0'/><Parameter name='P9' type='float' defaultValue='0'/><Parameter name='P10' type='float' defaultValue='0'/><Parameter name='P11' type='float' defaultValue='0'/>\
<Parameter name='P12' type='float' defaultValue='0'/><Parameter name='P13' type='float' defaultValue='0'/><Parameter name='P14' type='float' defaultValue='0'/><Parameter name='P15' type='float' defaultValue='0'/>\
<Parameter name='P16' type='float' defaultValue='0'/><Parameter name='P17' type='float' defaultValue='0'/><Parameter name='P18' type='float' defaultValue='0'/><Parameter name='P19' type='float' defaultValue='0'/>\
<Parameter name='P20' type='float' defaultValue='0'/><Parameter name='P21' type='float' defaultValue='0'/><Parameter name='P22' type='float' defaultValue='0'/></Model></Category>\
<Category id='10' name='Splitter' hidden='true'><Model id='10004' name='Splitter'>\
<Parameter name='TYPE' type='switch' min='0' max='2' defaultValue='0'/></Model></Category>\
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
<Module id='3' name='Huge' blob_name='huge' category='0' dm_heap='100' pm_heap='10' \
sd_heap='0' dma='0' dm='10' pm='0' sw='100' nw='0' Latency='0'>\
<Test name='Stimulus High' avg='10'/></Module>\
<Module id='2' name='Echo' blob_name='echo' category='6' dm_heap='100' pm_heap='10' \
sd_heap='0' dma='0' dm='10' pm='0' sw='100' nw='0' Latency='0'>\
<Test name='Stimulus High' avg='10'/></Module>\
<Module id='5018' name='Comp' blob_name='comp' category='5' dm_heap='100' pm_heap='10' \
sd_heap='0' dma='0' dm='10' pm='0' sw='100' nw='0' Latency='0'>\
<Test name='Stimulus High' avg='5'/></Module>\
<Module id='16008' name='Blend' blob_name='blend' category='16' dm_heap='100' pm_heap='10' \
sd_heap='0' dma='0' dm='10' pm='0' sw='100' nw='0' Latency='0'>\
<Test name='Stimulus High' avg='5'/></Module>\
<Module id='12000' name='Cab' blob_name='cab' category='12' dm_heap='100' pm_heap='10' \
sd_heap='0' dma='0' dm='10' pm='0' sw='100' nw='0' Latency='0'>\
<Test name='Stimulus High' avg='10'/></Module>\
<Module id='26001' name='Freeze' blob_name='freeze' category='26' dm_heap='100' pm_heap='10' \
sd_heap='0' dma='0' dm='10' pm='0' sw='100' nw='0' Latency='0'>\
<Test name='Stimulus High' avg='10'/></Module>\
<Module id='18007' name='Minivoicer' blob_name='minivoicer' category='18' dm_heap='100' \
pm_heap='10' sd_heap='0' dma='0' dm='10' pm='0' sw='100' nw='0' Latency='0'>\
<Test name='Stimulus High' avg='10'/></Module>\
</ModuleReport>";

const FIXTURE: Fixture = Fixture {
    models: MODELS,
    report: REPORT,
    blob: |name| match name {
        "huge" => huge_blob(),
        _ => light_blob(),
    },
    diffs: &[
        "light",
        "echo",
        "comp",
        "cab",
        "huge",
        "blend",
        "freeze",
        "minivoicer",
    ],
};

/// [`light_blob`] plus a DM section that overflows the core's DM pool (`blob::place`).
fn huge_blob() -> ModuleBlob {
    const DM: usize = 0xe801;
    let mut blob = light_blob();
    blob.contents.push(SectionBytes {
        section: Section::Dm,
        bytes: vec![0; DM],
    });
    blob.ranges
        .push(LinkRange::for_contents(Section::Dm, DM).unwrap());
    blob
}

const PRESET_PATH: &str = "/media/p4/Presets/My Presets/Light.pb";
const LIGHT: (u8, u8) = (0, 2);
const ECHO: (u8, u8) = (0, 4);
const CAB: (u8, u8) = (0, 5);
const COMP: (u8, u8) = (0, 6);

fn slot((row, column): (u8, u8)) -> Slot {
    Slot {
        row: Row::new(row).unwrap(),
        column: Column::new(column).unwrap(),
    }
}

/// The booted DSP, the catalog, the library and the deployed preset.
struct Rig {
    sim: Sim,
    dsp: Dsp<Sim>,
    specs: Specs,
    library: StockLibrary,
    current: Current,
    log: Rc<RefCell<Log>>,
    _media: Media,
}

impl Rig {
    /// Boots, loads the catalog with the DSP key, reads slot 0 and deploys it through
    /// `Dsp::apply`.
    fn deploy(edit: impl FnOnce(&mut Preset, &SpecsCatalog)) -> (Self, dsp::Pending) {
        let specs = FIXTURE.specs();
        let catalog = SpecsCatalog(&specs);
        let mut preset = Preset::new_default("4.1.0", &catalog).unwrap();
        edit(&mut preset, &catalog);
        let media = Media::new([("Light", preset)]);
        let log = Rc::new(RefCell::new(Log::default()));
        let mut sim = Sim::new(FIXTURE, &media, &log);
        let booted = dsp::boot(&mut sim, Variant::QuadCortex).unwrap();
        let repo = sim.dsp_file(DspFile::ModelRepo).unwrap();
        let report = sim.dsp_file(DspFile::ModuleReport).unwrap();
        let loaded = preset::load_catalog(&repo, &report, booted.repo_key.as_ref().unwrap());
        assert_eq!(loaded.unwrap().specs.models.len(), specs.models.len());
        let library = StockLibrary::open(MediaRoot::new(&media.0), StockLimits::default()).unwrap();
        let path = preset::last_preset_path(&library, None, 0).unwrap();
        let read = preset::read(&library, path.as_deref(), &specs).unwrap();
        let prepared = preset::prepare(read, &specs, false, None, Default::default()).unwrap();
        assert!(matches!(prepared.update, Update::Redeploy { .. }));
        let mut dsp = booted.dsp;
        let context = context(&specs, &prepared.deployment);
        let pending = dsp.apply(&mut sim, &prepared.update, &context).unwrap();
        let current = Current {
            path,
            preset: prepared.preset,
            migration: prepared.migration,
            deployment: prepared.deployment,
        };
        let rig = Self {
            sim,
            dsp,
            specs,
            library,
            current,
            log,
            _media: media,
        };
        (rig, pending)
    }

    fn mark(&self) -> usize {
        self.log.borrow().requests.len()
    }

    /// Frames the DSP consumed since `mark`, after 1 ms for the sends without a wait.
    fn since(&self, mark: usize) -> Vec<Vec<u8>> {
        self.sim.clock.clone().sleep(Duration::from_millis(1));
        self.log.borrow().requests[mark..]
            .iter()
            .map(|sent| sent.frame.clone())
            .collect()
    }

    fn apply(&mut self, update: &Update) -> dsp::Pending {
        self.try_apply(update, None).unwrap()
    }

    /// Applies `update` with `deployment` (default: the committed one) as the context.
    fn try_apply(
        &mut self,
        update: &Update,
        deployment: Option<&Deployment>,
    ) -> Result<dsp::Pending, DspError> {
        let deployment = deployment.unwrap_or(&self.current.deployment);
        let context = context(&self.specs, deployment);
        self.dsp.apply(&mut self.sim, update, &context)
    }

    /// An edit of the committed layout that is not committed (a redeploy candidate).
    fn edited(&self, at: (u8, u8), model: u32) -> (Deployment, Update) {
        let mut next = self.current.deployment.clone();
        let update = next
            .edit(&self.specs, |layout| {
                layout.add_block(slot(at), ModelId(model), &self.specs)
            })
            .unwrap();
        (next, update)
    }

    /// Loading the committed layout again: a same-layout reuse (`0x5848b0` result 1).
    fn reload(&mut self) -> Update {
        let layout = self.current.deployment.layout().clone();
        let update = self.current.deployment.load(&self.specs, layout).unwrap();
        assert!(matches!(update, Update::Reuse(_)));
        update
    }

    fn placement(&self, at: (u8, u8)) -> Placement {
        self.current.deployment.plan().block(slot(at)).unwrap()
    }
}

fn stock_settings() -> tetrad_settings::device::Settings {
    tetrad_settings::device::Settings::defaults(tetrad_settings::device::Hardware::QuadCortex)
}

/// The stock-default context (`Context::new`) with the tuner port at 0, so new context fields
/// need no change here.
fn context<'a>(specs: &'a Specs, deployment: &'a Deployment) -> Context<'a> {
    Context {
        tuner_input: 0,
        ..Context::new(specs, deployment, &stock_settings())
    }
}

fn word(frame: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(frame[at..at + 4].try_into().unwrap())
}

/// A rig with a Looper X (27000) deployed at (0, 2) over made-up Looper tables, and the full-size
/// QC Looper context it was deployed with.
fn looper_rig() -> (Rig, tetrad_dsp_host::setup::drivers::looper::LooperContext) {
    use tetrad_dsp_host::setup::drivers::looper::{
        LooperClock, LooperContext, LooperMidiClockStart,
    };
    use tetrad_dsp_host::setup::{StockPayload, StockTables, TableError};
    struct MadeUpTables;
    impl StockTables for MadeUpTables {
        fn read(&self, payload: StockPayload) -> Result<Vec<u8>, TableError> {
            Ok((0..payload.len() / 4)
                .flat_map(|index| ((index + 2) as u32).to_le_bytes())
                .collect())
        }
    }
    let (mut rig, _) = Rig::deploy(|_, _| {});
    let params: String = (0..23)
        .map(|index| {
            format!("<Parameter name='P{index}' type='float' min='0' max='1' defaultValue='0'/>")
        })
        .collect();
    let models = MODELS.replace("</Models>", &format!("<Category id='27' name='Looper'><Model id='27000' name='Loop' blob='loop'>{params}</Model></Category></Models>"));
    let report = REPORT.replace("</ModuleReport>", "<Module id='27000' name='Loop' blob_name='loop' category='27' dm_heap='100' pm_heap='10' sd_heap='0' dma='0' dm='10' pm='0' sw='100' nw='0' Latency='0'><Test name='Stimulus High' avg='10'/></Module></ModuleReport>");
    rig.specs = Specs::resolve(&Catalog::parse(&models, &report).unwrap()).unwrap();
    rig.dsp.load_looper_tables(&MadeUpTables).unwrap();
    let looper_context = LooperContext {
        mode_changes_allowed: Some(true),
        clock: Some(LooperClock {
            metronome_slot_4: 0.0,
            midi_gate_f514: false,
            midi_gate_fbb4: false,
        }),
        midi_clock_start: Some(LooperMidiClockStart {
            enabled: false,
            inhibited: false,
        }),
    };
    let change = preset::prepare_layout_edit(
        &rig.current,
        &rig.specs,
        tetrad_app::device::actions::LayoutEdit::Add {
            slot: slot((0, 2)),
            model: 27000,
        },
    )
    .unwrap();
    let mut push = context(&rig.specs, change.deployment());
    push.looper_context = looper_context;
    let pending = rig.dsp.apply(&mut rig.sim, &change.update, &push).unwrap();
    assert!(pending.0.is_empty(), "{pending:?}");
    change.commit(&mut rig.current);
    (rig, looper_context)
}

/// `0x55e140` / `0x55d85c`: Looper P8 bypasses 0x02, adopts status and polls at 50 ms.
#[test]
fn looper_uses_its_driver_for_parameters_status_and_polling() {
    use tetrad_dsp_host::setup::drivers::looper::STATUS_PERIOD;
    use tetrad_dsp_protocol::responses::LooperStatus;
    let (mut rig, looper_context) = looper_rig();
    assert!(rig.dsp.looper_playback().adopt_next_state);
    let status = LooperStatus {
        state: 1,
        loop_length: 100,
        in_reverse: 1,
        ..LooperStatus::from_words([0; 22])
    };
    rig.dsp.looper_status(&status, &looper_context).unwrap();
    assert_eq!(rig.dsp.looper_playback().cached_state, 1);
    assert!(!rig.dsp.looper_playback().adopt_next_state);
    let mark = rig.mark();
    let site = Site::Slot(slot((0, 2)));
    let update = preset::set_parameter(&mut rig.current, &rig.specs, site, 8, 0.75).unwrap();
    let mut push = context(&rig.specs, &rig.current.deployment);
    push.looper_context = looper_context;
    rig.dsp.apply(&mut rig.sim, &update, &push).unwrap();
    let frames = rig.since(mark);
    assert!(
        frames
            .iter()
            .any(|frame| frame[0] == 0x2a && word(frame, 4) == 0)
    );
    assert!(frames.iter().all(|frame| frame[0] != 0x02));
    // Record while reversed: the rig P7 reset waits for the reducer, then event 3.
    let mark = rig.mark();
    rig.dsp
        .looper_parameter(site, 0, 1.0, &looper_context)
        .unwrap();
    assert_eq!(rig.dsp.take_rig_writes(), [(slot((0, 2)), 7, 0.0)]);
    assert!(
        rig.since(mark)
            .iter()
            .any(|frame| frame[0] == 0x29 && word(frame, 16) == 3),
        "{:x?}",
        rig.since(mark)
    );
    assert_eq!(STATUS_PERIOD, Duration::from_millis(50));
    let mark = rig.mark();
    rig.dsp.poll_loopers(true).unwrap();
    assert!(rig.since(mark).is_empty());
    rig.dsp.poll_loopers(false).unwrap();
    assert!(rig.since(mark).iter().any(|frame| frame[0] == 0x2b));
}

/// `0x50eef8` / `0x50f360`: a redeploy without a 27000 tears the old Looper X down globally
/// (both insert paths disconnected), then polls track 0 unless `nopoll`; a kept 27000 tears
/// down locally and polls.
#[test]
fn looper_redeploy_picks_the_teardown_branch_and_status_request() {
    use tetrad_app::device::actions::LayoutEdit;
    let looper_frames = |edit, nopoll| {
        let (mut rig, looper_context) = looper_rig();
        let change = preset::prepare_layout_edit(&rig.current, &rig.specs, edit).unwrap();
        assert!(matches!(change.update, Update::Redeploy { .. }));
        let mut push = context(&rig.specs, change.deployment());
        push.looper_context = looper_context;
        push.nopoll = nopoll;
        let mark = rig.mark();
        rig.dsp.apply(&mut rig.sim, &change.update, &push).unwrap();
        rig.since(mark)
            .into_iter()
            .filter(|frame| frame[0] == 0x2b || (frame[0] == 0x2c && word(frame, 16) == u32::MAX))
            .map(|frame| frame[0])
            .collect::<Vec<_>>()
    };
    let replace = || LayoutEdit::Replace {
        slot: slot((0, 2)),
        model: 1,
    };
    assert_eq!(looper_frames(replace(), false), [0x2c, 0x2c, 0x2b]);
    assert_eq!(looper_frames(replace(), true), [0x2c, 0x2c]);
    let keep = LayoutEdit::Add {
        slot: slot((0, 3)),
        model: 1,
    };
    assert_eq!(looper_frames(keep, false), [0x2b]);
}

/// `{slot, f32 bits}` records of a `0x3d` BulkModelBypass frame.
/// A core 0 silent after the handshake (about one QC boot in ten, 2026-10-15) gets one more
/// SOC1 restart; the second boot finishes with the keys. `--drill-core0` forces the first.
#[test]
fn a_silent_core0_gets_one_more_soc1_restart() {
    let catalog = SpecsCatalog(&FIXTURE.specs());
    let media = Media::new([("Light", Preset::new_default("4.1.0", &catalog).unwrap())]);
    let log = Rc::new(RefCell::new(Log::default()));
    let mut sim = Sim::new(FIXTURE, &media, &log);
    sim.drill_core0 = true;
    let booted = dsp::boot(&mut sim, Variant::QuadCortex).unwrap();
    assert!(booted.soc1_retried);
    assert!(booted.repo_key.is_some());
}

fn bypass_records(frame: &[u8]) -> Vec<(u32, u32)> {
    frame[16..]
        .chunks_exact(8)
        .map(|record| (word(record, 0), word(record, 4)))
        .collect()
}

fn insert(preset: &mut Preset, catalog: &SpecsCatalog, at: (u8, u8), id: u32, values: &[f32]) {
    let block = Model::block(id, values.iter().map(|&value| ParamValue::Float(value)));
    let slot = tetrad_preset::Slot {
        row: usize::from(at.0),
        column: usize::from(at.1),
    };
    preset.insert_block(slot, block, catalog).unwrap();
}

/// Stock deploy (`0x585464`): mute, 20 ms, run-list start, then the push in bulk `Value16`
/// with router channel words as `0x03` ints (`0x581920`); a block without a recovered driver
/// is held in a muted bypass and reported, never pushed unset.
#[test]
fn synthetic_preset_deploys_with_per_model_setup() {
    let (rig, pending) = Rig::deploy(|preset, catalog| {
        insert(preset, catalog, LIGHT, 1, &[0.5, 0.0]);
        insert(preset, catalog, CAB, 12000, &[0.5, 0.0]);
    });
    let frames = rig.since(0);
    let kind = |predicate: &dyn Fn(&Vec<u8>) -> bool| frames.iter().position(predicate).unwrap();
    let mute = kind(&|f| f[0] == 0x20 && word(f, 16) == 1);
    let start = kind(&|f| f[0] == 0x1c);
    let push = kind(&|f| f[0] == 0x1d);
    let unmute = frames
        .iter()
        .rposition(|f| f[0] == 0x20 && word(f, 16) == 0);
    assert!(mute < start && start < push && Some(push) < unmute.map(|at| at + 1));
    assert_eq!(word(&frames[push], 4), 16, "bulk Value16 after a redeploy");
    let router = frames[start..]
        .iter()
        .find(|f| f[0] == 0x03)
        .expect("router channel words go out as 0x03");
    assert_eq!(word(router, 4), 0, "channel type first");

    let cab = rig.placement(CAB);
    let records: Vec<_> = frames
        .iter()
        .filter(|f| f[0] == 0x3d && usize::from(f[1]) == cab.core.index())
        .flat_map(|f| bypass_records(f))
        .collect();
    assert!(records.contains(&(cab.slot.index() as u32, MUTED_BYPASS)));
    let parameters_to_cab = frames
        .iter()
        .filter(|f| f[0] == 0x1d && usize::from(f[1]) == cab.core.index())
        .flat_map(|f| f[16..].chunks_exact(12).map(|record| word(record, 0)))
        .any(|slot| slot == cab.slot.index() as u32);
    assert!(!parameters_to_cab, "a muted block gets no values");
    assert!(
        pending
            .0
            .iter()
            .any(|step| step.contains("model 12000 muted"))
    );
}

/// `addModelToChain` `0x00522d70` → `0x00521c84`: a Grid plugin block whose licence status is
/// not 2 deploys bypassed; a block without a `plugin_id` keeps its stored bypass.
#[test]
fn unlicensed_plugin_block_deploys_bypassed() {
    let (mut rig, _) = Rig::deploy(|preset, catalog| {
        insert(preset, catalog, LIGHT, 1, &[0.5, 0.0]);
        insert(preset, catalog, ECHO, 2, &[0.3, 0.5, 0.0]);
    });
    let (light, echo) = (rig.placement(LIGHT), rig.placement(ECHO));
    rig.dsp
        .set_licences(tetrad_app::device::services::Licences::default());
    let mark = rig.mark();
    let (next, update) = rig.edited(COMP, 5018);
    rig.try_apply(&update, Some(&next)).unwrap();
    let records: Vec<_> = rig
        .since(mark)
        .iter()
        .filter(|f| f[0] == 0x3d)
        .flat_map(|f| bypass_records(f))
        .collect();
    assert!(
        records.contains(&(light.slot.index() as u32, BYPASSED)),
        "{records:?}"
    );
    assert!(
        records.contains(&(echo.slot.index() as u32, 0)),
        "{records:?}"
    );
}

/// (O) Issue #17: a reload with no licence for Light's plugin keeps Light on the grid, loads
/// nothing for it and deploys the rest of the preset.
#[test]
fn unlicensed_plugin_block_loads_no_dsp_code() {
    let (mut rig, _) = Rig::deploy(|preset, catalog| {
        insert(preset, catalog, LIGHT, 1, &[0.5, 0.0]);
        insert(preset, catalog, ECHO, 2, &[0.3, 0.5, 0.0]);
    });
    let dormant = preset::dormant_models(&rig.specs, &Licences::default());
    let prepared = preset::prepare(
        rig.current.preset.clone(),
        &rig.specs,
        false,
        Some(&rig.current.deployment),
        dormant,
    )
    .unwrap();
    assert!(matches!(prepared.update, Update::Redeploy { .. }));
    rig.try_apply(&prepared.update, Some(&prepared.deployment))
        .unwrap();
    let deployment = &prepared.deployment;
    assert!(
        deployment
            .layout()
            .instance(Site::Slot(slot(LIGHT)))
            .is_some()
    );
    assert!(deployment.plan().block(slot(LIGHT)).is_none());
    assert!(deployment.plan().block(slot(ECHO)).is_some());
}

/// M5 (`0x879100`): two blocks that share a blob name on one core load it once, so both
/// slots start at the same entry (`0x1c` record `+0`).
#[test]
fn shared_blobs_load_once_per_core() {
    let (rig, _) = Rig::deploy(|preset, catalog| {
        insert(preset, catalog, LIGHT, 1, &[0.5, 0.0]);
        insert(preset, catalog, ECHO, 1, &[0.5, 0.0]);
    });
    let (first, second) = (rig.placement(LIGHT), rig.placement(ECHO));
    assert_eq!(first.core, second.core);
    let frames = rig.since(0);
    let start = frames
        .iter()
        .find(|f| f[0] == 0x1c && usize::from(f[1]) == first.core.index())
        .unwrap();
    let entry = |at: Placement| word(start, 16 + 12 * at.slot.index());
    assert_eq!(entry(first), entry(second));
    assert!(entry(first) > 22_004, "a loaded entry, not a built-in id");
}

/// The sidechain step (`0x585f04` → `0x5843a4`) runs after the lane step inside the bulk
/// window: a grid source on the sink's core sends `0x49` to cores 0..=3 (`+2` channel, `+4`
/// source slot, `+12` source core) before the flush, then the sink router gets kind 12 and its
/// channel, and its queued level is the scene's `sidechain_in`.
#[test]
fn block_sidechain_routes_inside_the_bulk_window() {
    let (rig, _) = Rig::deploy(|preset, catalog| {
        insert(preset, catalog, LIGHT, 1, &[0.5, 0.0]);
        // SOURCE 1.0 = step 14, the first preceding block.
        insert(preset, catalog, COMP, 5018, &[0.5, 1.0, 0.25, 0.0]);
    });
    let plan = rig.current.deployment.plan();
    let light = rig.placement(LIGHT);
    let sink = plan
        .placement(Node::Receive(Endpoint::Sidechain(slot(COMP))))
        .unwrap();
    assert_eq!(sink.core, light.core);
    let frames = rig.since(0);
    let routes: Vec<&Vec<u8>> = frames.iter().filter(|f| f[0] == 0x49).collect();
    assert_eq!(routes.len(), 4);
    for (core, frame) in routes.iter().enumerate() {
        assert!(routes.iter().any(|f| usize::from(f[1]) == core));
        assert_eq!(frame[2], 0, "channel");
        assert_eq!(word(frame, 4), light.slot.index() as u32);
        assert_eq!(word(frame, 12), light.core.index() as u32);
    }
    // The log keeps send order per ring only: check the sink core's ring.
    let ring: Vec<&Vec<u8>> = (frames.iter())
        .filter(|f| usize::from(f[1]) == sink.core.index())
        .collect();
    let at = |frame: &Vec<u8>, place: Placement| usize::from(frame[2]) == place.slot.index();
    let lane_routers: Vec<Placement> = (plan.routers.lane_input.iter())
        .chain(&plan.routers.lane_output)
        .flatten()
        .filter(|router| router.core == sink.core)
        .copied()
        .collect();
    let lane_step = ring
        .iter()
        .rposition(|f| f[0] == 0x03 && lane_routers.iter().any(|&router| at(f, router)))
        .unwrap();
    let route = ring.iter().position(|f| f[0] == 0x49).unwrap();
    let flush = ring.iter().position(|f| f[0] == 0x1d).unwrap();
    assert!(lane_step < route && route < flush);
    let sink_ints: Vec<(u32, u32)> = ring[route..flush]
        .iter()
        .filter(|f| f[0] == 0x03 && at(f, sink))
        .map(|f| (word(f, 4), word(f, 16)))
        .collect();
    // Step 6 on the SOC1 source router (the same 22000), then step 7: stereo, kind, channel,
    // active.
    assert_eq!(
        sink_ints,
        [(0, 12), (1, 0), (2, 1), (0, 12), (1, 0), (5, 1)]
    );
    let level = frames
        .iter()
        .filter(|f| f[0] == 0x1d && usize::from(f[1]) == sink.core.index())
        .flat_map(|f| {
            f[16..]
                .chunks_exact(12)
                .map(|r| (word(r, 0), word(r, 4), word(r, 8)))
        })
        .any(|record| record == (sink.slot.index() as u32, 8, 0.25_f32.to_bits()));
    assert!(level, "router 8 = the sidechain_in value");
}

/// A `sidechain_in` edit (`0x51ee94`) re-routes the sink router only (mode 2), at once: no
/// route request, the router ints, then the new level as router parameter 8.
#[test]
fn sidechain_level_edit_routes_the_sink_router_only() {
    let (mut rig, _) = Rig::deploy(|preset, catalog| {
        insert(preset, catalog, LIGHT, 1, &[0.5, 0.0]);
        insert(preset, catalog, COMP, 5018, &[0.5, 1.0, 0.25, 0.0]);
    });
    let sink = (rig.current.deployment.plan())
        .placement(Node::Receive(Endpoint::Sidechain(slot(COMP))))
        .unwrap();
    let mark = rig.mark();
    let update = preset::set_parameter(
        &mut rig.current,
        &rig.specs,
        Site::Slot(slot(COMP)),
        2,
        0.75,
    )
    .unwrap();
    rig.apply(&update);
    let writes: Vec<(u8, u32, u32)> = rig
        .since(mark)
        .into_iter()
        .filter(|f| {
            (usize::from(f[1]), usize::from(f[2])) == (sink.core.index(), sink.slot.index())
                && matches!(f[0], 0x02 | 0x03 | 0x49)
        })
        .map(|f| (f[0], word(&f, 4), word(&f, 16)))
        .collect();
    let float = |value: f32| value.to_bits();
    assert_eq!(
        writes,
        [
            (0x03, 2, 1),
            (0x03, 0, 12),
            (0x03, 1, 0),
            (0x02, 7, float(1.0)),
            (0x02, 9, float(1.0)),
            (0x02, 8, float(0.75)),
            (0x03, 5, 1),
        ]
    );
}

/// A live edit is one `0x02` to the block's slot with the stored value (`0x51e4d8`).
#[test]
fn parameter_edit_sends_one_parameter_change() {
    let (mut rig, _) =
        Rig::deploy(|preset, catalog| insert(preset, catalog, LIGHT, 1, &[0.5, 0.0]));
    let at = rig.placement(LIGHT);
    let mark = rig.mark();
    let update = preset::set_parameter(
        &mut rig.current,
        &rig.specs,
        Site::Slot(slot(LIGHT)),
        0,
        0.75,
    )
    .unwrap();
    rig.apply(&update);
    let frames = rig.since(mark);
    assert_eq!(frames.len(), 1);
    let frame = &frames[0];
    assert_eq!((frame[0], usize::from(frame[2])), (0x02, at.slot.index()));
    assert_eq!((word(frame, 4), word(frame, 16)), (0, 0.75_f32.to_bits()));
}

/// A bypass toggle is one `0x0c` with 1.0 (`0x517228`), sent at once.
#[test]
fn bypass_toggle_sends_model_bypass() {
    let (mut rig, _) =
        Rig::deploy(|preset, catalog| insert(preset, catalog, LIGHT, 1, &[0.5, 0.0]));
    let at = rig.placement(LIGHT);
    let mark = rig.mark();
    let update =
        preset::set_bypass(&mut rig.current, &rig.specs, Site::Slot(slot(LIGHT)), true).unwrap();
    rig.apply(&update);
    let frames = rig.since(mark);
    let bypasses: Vec<_> = frames.iter().filter(|f| f[0] == 0x0c).collect();
    assert_eq!(bypasses.len(), 1);
    assert_eq!(usize::from(bypasses[0][2]), at.slot.index());
    assert_eq!(word(bypasses[0], 16), BYPASSED);
}

/// Scene recall (`0x527248`) in bulk `Value8`: every block's values queue before the bypass
/// records, blocks go in grid order with the new scene's bypass, and MUTE/BYPASS never go
/// out as `0x02`. Without a cab block the first pass (categories 12/21/32/33) is empty.
#[test]
fn scene_recall_follows_stock_order() {
    let (mut rig, _) = Rig::deploy(|preset, catalog| {
        insert(preset, catalog, LIGHT, 1, &[0.5, 0.0]);
        insert(preset, catalog, ECHO, 2, &[0.3, 0.5, 0.0]);
        let bypass = preset
            .block_bypass_mut(tetrad_preset::Slot { row: 0, column: 2 })
            .unwrap();
        bypass.set_scene_mode(true, 0).unwrap();
        bypass.set_bypassed(1, true).unwrap();
    });
    let (light, echo) = (rig.placement(LIGHT), rig.placement(ECHO));
    let mark = rig.mark();
    let _ = rig
        .current
        .deployment
        .select_scene(SceneIndex::new(1).unwrap());
    rig.dsp
        .recall_scene(&context(&rig.specs, &rig.current.deployment), None)
        .unwrap();
    let core = light.core.index() as u8;
    let frames: Vec<_> = rig
        .since(mark)
        .into_iter()
        .filter(|f| f[1] == core && matches!(f[0], 0x1d | 0x3d))
        .collect();
    assert_eq!(
        frames.iter().map(|f| f[0]).collect::<Vec<_>>(),
        [0x1d, 0x3d]
    );
    assert_eq!(word(&frames[0], 4), 8, "bulk Value8 in a recall");
    let indices = |at: Placement| -> Vec<u32> {
        frames[0][16..]
            .chunks_exact(12)
            .filter(|record| word(record, 0) == at.slot.index() as u32)
            .map(|record| word(record, 4))
            .collect()
    };
    assert_eq!((indices(light), indices(echo)), (vec![0], vec![0, 1]));
    assert_eq!(
        bypass_records(&frames[1]),
        [
            (light.slot.index() as u32, BYPASSED),
            (echo.slot.index() as u32, 0)
        ]
    );
}

/// `0x1f` EnableAll frames as (core, value).
fn enables(frames: &[Vec<u8>]) -> Vec<(u8, u32)> {
    frames
        .iter()
        .filter(|f| f[0] == 0x1f)
        .map(|f| (f[1], word(f, 4)))
        .collect()
}

/// Review F1/F5: both deploy paths of `0x585464` end with `0x58276c` (`0x1f` 1 to cores
/// 0..=3) after the push, before the unmute. Both send the gate (`0x30`) and output EQ
/// (`0x3b`) enables; only the redeploy sends `0x19`; the reuse starts with `0x1f` 0.
#[test]
fn deploy_and_reuse_end_by_enabling_every_core() {
    let (mut rig, _) =
        Rig::deploy(|preset, catalog| insert(preset, catalog, LIGHT, 1, &[0.5, 0.0]));
    let count = |frames: &[Vec<u8>], kind: u8| frames.iter().filter(|f| f[0] == kind).count();
    // Core 0's ring keeps send order: the last bulk flush, the enable, then the unmute.
    let ends_enabled = |frames: &[Vec<u8>]| {
        let ring: Vec<&Vec<u8>> = frames.iter().filter(|f| f[1] == 0).collect();
        let last = |kind: u8| ring.iter().rposition(|f| f[0] == kind).unwrap();
        last(0x1d) < last(0x1f) && last(0x1f) < last(0x20) && word(ring[last(0x1f)], 4) == 1
    };
    let deployed = rig.since(0);
    let mut started: Vec<(u8, u32)> = enables(&deployed);
    started.sort_unstable();
    assert_eq!(started, [(0, 1), (1, 1), (2, 1), (3, 1)]);
    assert!(ends_enabled(&deployed));
    assert_eq!((count(&deployed, 0x19), count(&deployed, 0x3b)), (1, 1));
    assert!(count(&deployed, 0x30) > 0);

    let update = rig.reload();
    let mark = rig.mark();
    rig.apply(&update);
    let reused = rig.since(mark);
    let mut values: Vec<(u8, u32)> = enables(&reused);
    values.sort_unstable();
    assert_eq!(
        values,
        [
            (0, 0),
            (0, 1),
            (1, 0),
            (1, 1),
            (2, 0),
            (2, 1),
            (3, 0),
            (3, 1)
        ]
    );
    assert!(ends_enabled(&reused));
    assert_eq!((count(&reused, 0x19), count(&reused, 0x1e)), (0, 0));
    assert_eq!(count(&reused, 0x3b), 1);
    assert!(count(&reused, 0x30) > 0);
}

/// Review F2: a redeploy that does not fit sends nothing (`WontFit`) and keeps the running
/// blocks, so a recall still pushes the old block's values.
#[test]
fn wont_fit_keeps_the_running_blocks() {
    let (mut rig, _) =
        Rig::deploy(|preset, catalog| insert(preset, catalog, LIGHT, 1, &[0.5, 0.0]));
    let light = rig.placement(LIGHT);
    let (next, update) = rig.edited(ECHO, 3);
    let mark = rig.mark();
    let error = rig.try_apply(&update, Some(&next)).unwrap_err();
    assert!(matches!(error, DspError::WontFit { .. }), "{error}");
    let sent = rig.since(mark);
    assert!(!sent.iter().any(|f| matches!(f[0], 0x1e | 0x1c | 0x1f)));
    let mark = rig.mark();
    rig.dsp
        .recall_scene(&context(&rig.specs, &rig.current.deployment), None)
        .unwrap();
    let pushed = rig
        .since(mark)
        .iter()
        .filter(|f| f[0] == 0x1d && usize::from(f[1]) == light.core.index())
        .flat_map(|f| {
            f[16..]
                .chunks_exact(12)
                .map(|r| word(r, 0))
                .collect::<Vec<_>>()
        })
        .any(|slot| slot == light.slot.index() as u32);
    assert!(pushed, "the old block is still recalled");
}

/// Review F2: a deploy that fails after the run-list clear leaves the DSP out of sync. Edits
/// and recalls are refused; loading the running preset again (a reuse) redeploys it.
#[test]
fn failed_deploy_refuses_edits_until_a_redeploy() {
    let (mut rig, _) =
        Rig::deploy(|preset, catalog| insert(preset, catalog, LIGHT, 1, &[0.5, 0.0]));
    let (next, update) = rig.edited(ECHO, 2);
    // The deploy's `syncAllCores` times out after the run-list clear (one timeout kills no
    // core).
    rig.sim.clock.hold.set(Some(|ring, frame| {
        ring == CoreId::Dsp(DspCore::Core2) && frame[0] == 0x0a
    }));
    let failed = rig.try_apply(&update, Some(&next));
    rig.sim.clock.hold.set(None);
    let timed_out = matches!(
        failed,
        Err(DspError::Session(SessionError::SendTimeout { .. }))
    );
    assert!(timed_out, "{failed:?}");
    let edit = preset::set_parameter(
        &mut rig.current,
        &rig.specs,
        Site::Slot(slot(LIGHT)),
        0,
        0.75,
    )
    .unwrap();
    assert!(matches!(
        rig.try_apply(&edit, None),
        Err(DspError::OutOfSync)
    ));
    let recall = rig
        .dsp
        .recall_scene(&context(&rig.specs, &rig.current.deployment), None);
    assert!(matches!(recall, Err(DspError::OutOfSync)));
    let update = rig.reload();
    let mark = rig.mark();
    rig.apply(&update);
    assert!(
        rig.since(mark).iter().any(|f| f[0] == 0x1e),
        "the reuse redeploys"
    );
    rig.apply(&edit);
}

/// Review final-device D2: a reuse whose push completed commits even when a send stock
/// ignores fails. A `0x1f` (0) that finds no slot and a refused unmute become `Pending`
/// notes; the DSP stays in sync, so the next edit is sent.
#[test]
fn reuse_after_a_complete_push_stays_in_sync_through_send_failures() {
    let (mut rig, _) =
        Rig::deploy(|preset, catalog| insert(preset, catalog, LIGHT, 1, &[0.5, 0.0]));
    let edit = |rig: &mut Rig, value| {
        preset::set_parameter(
            &mut rig.current,
            &rig.specs,
            Site::Slot(slot(LIGHT)),
            0,
            value,
        )
        .unwrap()
    };

    let update = rig.reload();
    rig.sim
        .clock
        .hold
        .set(Some(|ring, _| ring == CoreId::Dsp(DspCore::Core3)));
    let enable = Request::EnableAll {
        core: DspCore::Core3,
        value: 1,
    };
    while rig.dsp.request(enable.clone()).is_ok() {}
    // The ring drains after the reuse's `0x1f` (0) gives up (mute, 20 ms settle, 100 ms slot
    // wait) and before setup's first core 3 send times out.
    rig.sim
        .clock
        .hold_until
        .set(Some(rig.sim.clock.now() + Duration::from_millis(150)));
    let pending = rig.try_apply(&update, None);
    rig.sim.clock.hold.set(None);
    rig.sim.clock.hold_until.set(None);
    let pending = pending.unwrap();
    assert!(
        pending
            .0
            .iter()
            .any(|note| note.starts_with("0x1f (0) to Core3")),
        "{pending:?}"
    );
    let update = edit(&mut rig, 0.25);
    rig.apply(&update);

    let update = rig.reload();
    rig.sim
        .clock
        .hold
        .set(Some(|_, frame| frame[0] == 0x20 && frame[16..20] == [0; 4]));
    let pending = rig.try_apply(&update, None);
    rig.sim.clock.hold.set(None);
    let pending = pending.unwrap();
    assert!(
        pending.0.iter().any(|note| note.starts_with("0x20 unmute")),
        "{pending:?}"
    );
    let update = edit(&mut rig, 0.75);
    rig.apply(&update);
}

/// Review r14 S-1 (r15 R9): the update install mutes (`0x4e63a4` → `0x876470(1, 1)`) and
/// holds the mute while the installer runs: a redeploy and a reuse end with the held value,
/// never an unmute. A failed install gives the inputs back and unmutes.
#[test]
fn install_mute_holds_through_redeploy_and_reuse() {
    use tetrad_app::device::Event;
    use tetrad_app::device::actions::Action;
    use tetrad_app::device::platform::Input;
    use tetrad_app::device::services::{JobError, ServiceNotice};
    use tetrad_app::device::ui::UiInput;
    use tetrad_ui::{Screen, SettingsPage};

    let specs = common::STOCK.specs();
    let media = Media::new([
        ("Light", common::light_preset(&specs, 2)),
        ("Second", common::light_preset(&specs, 3)),
    ]);
    let (mut device, log, _media) = common::boot(media);
    let steps =
        |device: &mut tetrad_app::device::Device<Sim>| (0..10).for_each(|_| _ = device.step());
    steps(&mut device);
    device.platform().worker.set(true);
    let queue = |device: &tetrad_app::device::Device<Sim>, notice| {
        device
            .platform()
            .inputs
            .borrow_mut()
            .push(Input::Service(notice));
    };
    queue(&device, ServiceNotice::Download(Ok(())));
    device.push(Event::UiInput(UiInput::Show(Screen::Settings(
        SettingsPage::Updates,
    ))));
    steps(&mut device);
    common::tap_update_install(&mut device, steps);
    assert!(device.bridges().services.inputs_masked());
    let last_mute = |mark: usize| {
        let log = log.borrow();
        let mute = log.requests[mark..]
            .iter()
            .rev()
            .find(|sent| sent.kind() == 0x20)
            .expect("a 0x20");
        word(&mute.frame, 16)
    };
    const HELD: u32 = 1 | 1 << 1;
    // The first load changes the layout (redeploy, `0x1e`); loading it again reuses it.
    for redeploy in [true, false] {
        let mark = log.borrow().requests.len();
        let target = PresetRef {
            setlist: None,
            index: 1,
        };
        let load = Action::LoadPreset {
            target,
            reload: true,
        };
        device.push(Event::Action(load));
        steps(&mut device);
        assert_eq!(log.borrow().kinds(mark).contains(&0x1e), redeploy);
        assert_eq!(last_mute(mark), HELD);
    }

    let mark = log.borrow().requests.len();
    queue(
        &device,
        ServiceNotice::Install(Err(JobError::WorkerStopped)),
    );
    steps(&mut device);
    assert!(!device.bridges().services.inputs_masked());
    assert_eq!(last_mute(mark), dsp::UNMUTED);
}

/// A DSP crash (`dspCrashSignal` → `0x5149dc`) shows the DSPErrorDialog and mutes while it
/// shows (owned audio safety). CANCEL (`0x514c48`) hides it, sets `LastPreset` to Factory
/// Library slot 0 (`0x64cea0`) with no restart, and the unit plays on as stock: the mute ends,
/// and a later deploy ends unmuted. A second crash shows the dialog again and mutes; REBOOT
/// (`0x514a6c`) writes the same `LastPreset` and asks the DSP fault restart (`0x6a8914(…, 2)`).
#[test]
fn a_dsp_crash_mutes_until_cancel_and_reboot_resets_the_last_preset() {
    use tetrad_app::device::actions::{Action, DspErrorChoice};
    use tetrad_app::device::{Event, FACTORY_SETLIST, PowerAction};
    use tetrad_dsp_host::responses::{CrashCause, Event as DspEvent};
    use tetrad_watchdog::power::RestartReason;

    let specs = common::STOCK.specs();
    let media = Media::new([
        ("Light", common::light_preset(&specs, 2)),
        ("Second", common::light_preset(&specs, 3)),
    ]);
    let (mut device, log, _media) = common::boot(media);
    let steps =
        |device: &mut tetrad_app::device::Device<Sim>| (0..10).for_each(|_| _ = device.step());
    steps(&mut device);
    let last_mute = |mark: usize| {
        let log = log.borrow();
        let mute = log.requests[mark..]
            .iter()
            .rev()
            .find(|sent| sent.kind() == 0x20)
            .expect("a 0x20");
        word(&mute.frame, 16)
    };

    let factory_slot_0 = |device: &tetrad_app::device::Device<Sim>| {
        let last = &device.state().settings.settings().last_preset;
        last.setlist.as_deref() == Some(FACTORY_SETLIST)
            && last.preset_index == 0
            && last.key.is_empty()
    };

    let mark = log.borrow().requests.len();
    let core = CoreId::Dsp(DspCore::Core2);
    let crash = DspEvent::Crash(CrashCause::SendTimeouts { core });
    device.push(Event::Dsp(core, crash.clone()));
    steps(&mut device);
    assert!(device.state().dsp_fault.is_some_and(|fault| fault.dialog));
    assert_eq!(last_mute(mark), dsp::DEPLOY_MUTE);

    let mark = log.borrow().requests.len();
    device.push(Event::Action(Action::DspErrorDialog(
        DspErrorChoice::Cancel,
    )));
    assert_eq!(device.step(), None, "CANCEL does not restart");
    steps(&mut device);
    assert!(device.state().dsp_fault.is_some_and(|fault| !fault.dialog));
    assert!(factory_slot_0(&device));
    assert_eq!(last_mute(mark), dsp::UNMUTED, "CANCEL plays on");

    let mark = log.borrow().requests.len();
    let target = PresetRef {
        setlist: None,
        index: 1,
    };
    device.push(Event::Action(Action::LoadPreset {
        target,
        reload: true,
    }));
    steps(&mut device);
    assert!(
        log.borrow().kinds(mark).contains(&0x1e),
        "the load redeploys"
    );
    assert_eq!(last_mute(mark), dsp::UNMUTED, "the load ends unmuted");
    assert!(
        !factory_slot_0(&device),
        "the load wrote its own LastPreset"
    );

    let mark = log.borrow().requests.len();
    device.push(Event::Dsp(core, crash));
    steps(&mut device);
    assert!(device.state().dsp_fault.is_some_and(|fault| fault.dialog));
    assert_eq!(
        last_mute(mark),
        dsp::DEPLOY_MUTE,
        "a second crash mutes again"
    );
    device.push(Event::Action(Action::DspErrorDialog(
        DspErrorChoice::Reboot,
    )));
    let restart = RestartReason::DspFault { delay_seconds: 2 };
    assert_eq!(device.step(), Some(PowerAction::Restart(restart)));
    assert!(device.state().dsp_fault.is_some_and(|fault| !fault.dialog));
    assert!(factory_slot_0(&device));
}

/// Review r23 K3: a fault that a deploy or a push raises (a module start failure, a fourth send
/// timeout) latches the fault mute before the end `0x20`, so that `0x20` mutes, not unmutes.
#[test]
fn a_fault_inside_a_deploy_or_push_ends_muted() {
    use tetrad_dsp_protocol::responses::{ResponseType, encode};
    let last_mute = |rig: &Rig, mark: usize| {
        let mute = rig
            .since(mark)
            .into_iter()
            .rev()
            .find(|frame| frame[0] == 0x20)
            .expect("a 0x20");
        word(&mute, 16)
    };

    // Deploy: a SOC2 core reports a model that failed to start (`0x8a1894`).
    let (mut rig, _) =
        Rig::deploy(|preset, catalog| insert(preset, catalog, LIGHT, 1, &[0.5, 0.0]));
    let (next, update) = rig.edited(ECHO, 2);
    let failed: Vec<u8> = [1_u32, 4, 0].iter().flat_map(|w| w.to_le_bytes()).collect();
    let core2 = CoreId::Dsp(DspCore::Core2);
    let frame = encode(ResponseType::ModelStart, 0, &failed);
    common::publish(&mut rig.sim.clock.rings.0.borrow_mut(), core2, &frame);
    let mark = rig.mark();
    _ = rig.try_apply(&update, Some(&next));
    assert!(rig.dsp.fault_muted());
    assert_eq!(last_mute(&rig, mark), dsp::DEPLOY_MUTE);

    // Push: core 3 has three timeouts; the reuse's `0x1f` (0) is the fourth (`0x89c680`).
    let (mut rig, _) =
        Rig::deploy(|preset, catalog| insert(preset, catalog, LIGHT, 1, &[0.5, 0.0]));
    let update = rig.reload();
    rig.sim
        .clock
        .hold
        .set(Some(|ring, _| ring == CoreId::Dsp(DspCore::Core3)));
    let enable = Request::EnableAll {
        core: DspCore::Core3,
        value: 1,
    };
    while rig.dsp.request(enable.clone()).is_ok() {}
    (0..2).for_each(|_| assert!(rig.dsp.request(enable.clone()).is_err()));
    let mark = rig.mark();
    _ = rig.try_apply(&update, None);
    rig.sim.clock.hold.set(None);
    assert!(rig.dsp.fault_muted());
    assert_eq!(last_mute(&rig, mark), dsp::DEPLOY_MUTE);
}

/// A sidechain SOURCE edit (`0x51e4d8`): the source route, the block bypass, the alignment
/// and the sink route (mode 2), then the selector `0x02` last.
#[test]
fn sidechain_source_edit_routes_before_the_selector() {
    let (mut rig, _) = Rig::deploy(|preset, catalog| {
        insert(preset, catalog, LIGHT, 1, &[0.5, 0.0]);
        insert(preset, catalog, COMP, 5018, &[0.5, 1.0, 0.25, 0.0]);
    });
    let comp = rig.placement(COMP);
    let sink = (rig.current.deployment.plan())
        .placement(Node::Receive(Endpoint::Sidechain(slot(COMP))))
        .unwrap();
    assert_eq!(sink.core, comp.core);
    let mark = rig.mark();
    let update =
        preset::set_parameter(&mut rig.current, &rig.specs, Site::Slot(slot(COMP)), 1, 0.0)
            .unwrap();
    rig.apply(&update);
    let ring: Vec<Vec<u8>> = (rig.since(mark).into_iter())
        .filter(|f| usize::from(f[1]) == comp.core.index())
        .collect();
    let at = |place: Placement| move |f: &&Vec<u8>| usize::from(f[2]) == place.slot.index();
    let bypass = ring
        .iter()
        .position(|f| f[0] == 0x0c && at(comp)(&f))
        .unwrap();
    let routes: Vec<usize> = (ring.iter().enumerate())
        .filter(|(_, f)| f[0] == 0x03 && at(sink)(f) && word(f, 4) == 0)
        .map(|(index, _)| index)
        .collect();
    let selector = ring
        .iter()
        .position(|f| f[0] == 0x02 && at(comp)(&f) && word(f, 4) == 1)
        .unwrap();
    assert_eq!(routes.len(), 2, "source route, then sink route");
    assert!(routes[0] < bypass && bypass < routes[1] && routes[1] < selector);
    assert_eq!(word(&ring[selector], 16), 0.0_f32.to_bits());
}

/// Review F3 and c-dsp3b4a F8: a SOURCE value goes through the 16008 driver (`step / 44`)
/// after the re-route, live and in a recall. The step comes from this block's list: one
/// preceding block gives 15 steps, so 1.0 is step 14.
#[test]
fn blend_source_goes_through_its_driver_with_the_block_steps() {
    let blend = (0, 6);
    let (mut rig, _) = Rig::deploy(|preset, catalog| {
        insert(preset, catalog, LIGHT, 1, &[0.5, 0.0]);
        insert(preset, catalog, blend, 16008, &[0.5; 7]);
    });
    let at = rig.placement(blend);
    let source_writes = |frames: &[Vec<u8>]| -> Vec<u32> {
        let to_block = |f: &&Vec<u8>| {
            (usize::from(f[1]), usize::from(f[2])) == (at.core.index(), at.slot.index())
        };
        let live = frames
            .iter()
            .filter(to_block)
            .filter(|f| f[0] == 0x02 && word(f, 4) == 6)
            .map(|f| word(f, 16));
        let bulk = frames
            .iter()
            .filter(|f| f[0] == 0x1d && usize::from(f[1]) == at.core.index())
            .flat_map(|f| {
                f[16..]
                    .chunks_exact(12)
                    .map(|r| (word(r, 0), word(r, 4), word(r, 8)))
                    .collect::<Vec<_>>()
            })
            .filter(|&(slot, index, _)| slot == at.slot.index() as u32 && index == 6)
            .map(|(_, _, bits)| bits);
        live.chain(bulk).collect()
    };
    let expected = (14.0_f32 / 44.0).to_bits();
    let mark = rig.mark();
    let update = preset::set_parameter(
        &mut rig.current,
        &rig.specs,
        Site::Slot(slot(blend)),
        6,
        1.0,
    )
    .unwrap();
    rig.apply(&update);
    assert_eq!(source_writes(&rig.since(mark)), [expected]);
    let mark = rig.mark();
    rig.dsp
        .recall_scene(&context(&rig.specs, &rig.current.deployment), None)
        .unwrap();
    assert_eq!(source_writes(&rig.since(mark)), [expected]);
}

/// Review F4: the recall tail (`0x527248`) sends the lane rows' values and the tempo inside
/// the bulk window, never the lane step's port words or the sidechain step's routes.
#[test]
fn scene_recall_sends_lane_values_not_the_lane_step() {
    let (mut rig, _) =
        Rig::deploy(|preset, catalog| insert(preset, catalog, LIGHT, 1, &[0.5, 0.0]));
    let plan = rig.current.deployment.plan();
    let output = plan
        .routers
        .lane_output
        .iter()
        .flatten()
        .next()
        .copied()
        .unwrap();
    let lane_routers: Vec<Placement> = (plan.routers.lane_input.iter())
        .chain(&plan.routers.lane_output)
        .flatten()
        .copied()
        .collect();
    let mark = rig.mark();
    rig.dsp
        .recall_scene(&context(&rig.specs, &rig.current.deployment), None)
        .unwrap();
    let frames = rig.since(mark);
    let to = |f: &Vec<u8>, place: Placement| {
        (usize::from(f[1]), usize::from(f[2])) == (place.core.index(), place.slot.index())
    };
    // Port words: input 0/1, output 0/1 (22001) or 3/4 (22002); index 9 is the alignment.
    let port_word = |f: &Vec<u8>| {
        f[0] == 0x03
            && matches!(word(f, 4), 0 | 1 | 3 | 4)
            && lane_routers.iter().any(|&router| to(f, router))
    };
    assert!(!frames.iter().any(port_word));
    assert!(!frames.iter().any(|f| matches!(f[0], 0x48 | 0x49)));
    let volume = frames
        .iter()
        .filter(|f| f[0] == 0x1d && usize::from(f[1]) == output.core.index())
        .flat_map(|f| {
            f[16..]
                .chunks_exact(12)
                .map(|r| (word(r, 0), word(r, 4)))
                .collect::<Vec<_>>()
        })
        .any(|record| record.0 == output.slot.index() as u32 && matches!(record.1, 3 | 5));
    assert!(
        volume,
        "the lane output router gets VOLUME in the bulk window"
    );
    assert_eq!(frames.iter().filter(|f| f[0] == 0x16).count(), 1, "tempo");
}

/// Save (`stamp_save` + `StockLibrary::save`): the edit persists, the preset is stamped as
/// changed in 4.1.0, and saving again writes nothing.
#[test]
fn save_round_trip_keeps_live_edits() {
    let (mut rig, _) =
        Rig::deploy(|preset, catalog| insert(preset, catalog, LIGHT, 1, &[0.5, 0.0]));
    preset::set_parameter(
        &mut rig.current,
        &rig.specs,
        Site::Slot(slot(LIGHT)),
        0,
        0.75,
    )
    .unwrap();
    let request = SaveRequest {
        slot: &PresetRef {
            setlist: None,
            index: 0,
        },
        name: "Light",
        now: SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000),
        account_name: "",
        account_id: "",
        creator: Creator::Quad,
    };
    let saved = preset::save(&mut rig.library, &mut rig.current, &rig.specs, &request).unwrap();
    assert_eq!((saved.path.as_str(), saved.written), (PRESET_PATH, true));
    let stored = Preset::try_from(&rig.library.load(PRESET_PATH).unwrap().document).unwrap();
    let block = stored
        .block(tetrad_preset::Slot { row: 0, column: 2 })
        .unwrap()
        .unwrap();
    assert_eq!(block.params[0].value(3), Some(&ParamValue::Float(0.75)));
    assert_eq!(stored.modified_version, ["4.1.0"]);
    assert_eq!(stored.date.as_deref(), Some("2023-11-14T22:13:20Z"));
    let again = preset::save(&mut rig.library, &mut rig.current, &rig.specs, &request).unwrap();
    assert!(!again.written);
}

/// The capture swap on the DSP (`0x528498`): user pushes are refused while a capture preset
/// runs, and the user plan deployed again ends the guard. The input override (`0x7d6440`)
/// is the jack scheduler's external mute (R15, next test).
#[test]
fn capture_swap_guards_user_pushes() {
    let (mut rig, _) =
        Rig::deploy(|preset, catalog| insert(preset, catalog, LIGHT, 1, &[0.5, 0.0]));
    let mut stored =
        capture::capture_preset(CapturePreset::Empty, Hardware::QuadCortex, &rig.specs, None)
            .unwrap();
    let (locked, update) = capture::compile(&mut stored, &rig.specs, false, true).unwrap();
    rig.dsp
        .apply_capture(&mut rig.sim, &update, &context(&rig.specs, &locked), true)
        .unwrap();

    let edit = preset::set_parameter(
        &mut rig.current,
        &rig.specs,
        Site::Slot(slot(LIGHT)),
        0,
        0.75,
    )
    .unwrap();
    assert!(matches!(
        rig.try_apply(&edit, None),
        Err(DspError::CapturePreset)
    ));

    let Update::Ops(ops) = rig
        .current
        .deployment
        .clone()
        .select_scene(SceneIndex::new(0).unwrap())
    else {
        unreachable!("a scene selection is values only")
    };
    let user = Update::Redeploy {
        clear: Vec::new(),
        plan: Box::new(rig.current.deployment.plan().clone()),
        ops,
    };
    let deployment = rig.current.deployment.clone();
    rig.dsp
        .apply_capture(
            &mut rig.sim,
            &user,
            &context(&rig.specs, &deployment),
            false,
        )
        .unwrap();
    rig.apply(&edit);
}

/// R15 (`0x7d6440`): the capture input override is the jack scheduler's one external mute
/// `+0xc`, not a DSP trim cache. Released while the scheduler's own gain mute `+0xd` holds,
/// it sends zeros; the trims go out only with the scheduler's unmute.
#[test]
fn capture_override_release_sends_trims_only_when_the_scheduler_is_unmuted() {
    let mut hal = RecordingHal::default();
    let mut scheduler = InputScheduler::new(HalPlatform::QcEss);
    let zeros = Effect::Dsp(DspCommand::InputTrims(None));
    let mut external = |scheduler: &mut InputScheduler, on| {
        scheduler
            .enqueue(InputChange::ExternalMute(on), &mut hal)
            .unwrap()
    };
    assert_eq!(external(&mut scheduler, true), std::slice::from_ref(&zeros));
    let plan = GainPlan::from_unit(HalPlatform::QcEss, 0.5).unwrap();
    let gain = InputChange::Gain {
        endpoint: GainEndpoint::Return1,
        plan,
        verification: tetrad_hal::analog::Verification::Unverified,
    };
    scheduler
        .enqueue(gain, &mut RecordingHal::default())
        .unwrap();
    assert_eq!(external(&mut scheduler, false), [zeros]);
    scheduler.on_timer();
    let trims = Effect::Dsp(DspCommand::InputTrims(Some([0.0, 0.0, plan.trim, 0.0])));
    assert!(scheduler.on_timer().contains(&trims));
}

/// `0x585464` with the capture lock (`+0x3409`, `+0x3411`): the gate enables (`0x5184fc`) and
/// `0x3b` OutputEqualizerEnable (`0x515034`) go out as 0; without it the placed gate is on.
#[test]
fn capture_lock_sends_the_gate_and_global_eq_enables_as_zero() {
    let (mut rig, _) =
        Rig::deploy(|preset, catalog| insert(preset, catalog, LIGHT, 1, &[0.5, 0.0]));
    let update = rig.reload();
    let enables = |rig: &mut Rig, capture_lock: bool| {
        let deployment = rig.current.deployment.clone();
        let context = Context {
            capture_lock,
            ..context(&rig.specs, &deployment)
        };
        let mark = rig.mark();
        rig.dsp.apply(&mut rig.sim, &update, &context).unwrap();
        let frames = rig.since(mark);
        let values = |kind: u8| -> Vec<u32> {
            frames
                .iter()
                .filter(|f| f[0] == kind)
                .map(|f| word(f, 16))
                .collect()
        };
        (values(0x30), values(0x3b))
    };
    let (gate, _) = enables(&mut rig, false);
    assert!(gate.iter().any(|&value| value != 0), "{gate:?}");
    assert_eq!(enables(&mut rig, true), (vec![0, 0], vec![0]));
}

/// Reuse `0x87d0d8`: after the push, every placed alignment delay router gets IntParameter 0
/// (`0x03`) before the alignment and the core enable; a redeploy sends no resets.
#[test]
fn reuse_zeroes_the_alignment_delays_before_the_alignment() {
    let (mut rig, _) =
        Rig::deploy(|preset, catalog| insert(preset, catalog, LIGHT, 1, &[0.5, 0.0]));
    let resets: Vec<(u8, u32, u32)> = rig
        .current
        .deployment
        .plan()
        .delay_resets()
        .iter()
        .map(|write| (write.at.slot.index() as u8, write.index, 0))
        .collect();
    assert!(!resets.is_empty());
    let int_writes = |frames: &[Vec<u8>]| -> Vec<(u8, u32, u32)> {
        frames
            .iter()
            .filter(|f| f[0] == 0x03)
            .map(|f| (f[2], word(f, 4), word(f, 16)))
            .collect()
    };
    let redeployed = rig.since(0);
    let update = rig.reload();
    let mark = rig.mark();
    rig.apply(&update);
    let reused = rig.since(mark);
    let last_bulk = reused.iter().rposition(|f| f[0] == 0x1d).unwrap();
    let first_enable = reused
        .iter()
        .position(|f| f[0] == 0x1f && word(f, 4) == 1)
        .unwrap();
    let tail = int_writes(&reused[last_bulk..first_enable]);
    assert_eq!(tail[..resets.len()], resets[..]);
    assert!(
        !int_writes(&redeployed)
            .windows(resets.len())
            .any(|window| window == resets),
        "a redeploy sends no resets"
    );
}

/// A stomp press on a Freeze block (`+0x40`, `0x55acd8`) toggles ON/OFF through its driver:
/// the `0x02` goes out at once and the write comes back as one echoed preset edit
/// (`0x52382c`) for the device model to store.
#[test]
fn freeze_stomp_press_writes_on_off_as_a_linked_edit() {
    let (mut rig, pending) =
        Rig::deploy(|preset, catalog| insert(preset, catalog, LIGHT, 26001, &[0.0, 0.0, 0.0]));
    assert!(pending.0.is_empty(), "{pending:?}");
    let at = rig.placement(LIGHT);
    let mark = rig.mark();
    let plan = rig.current.deployment.plan();
    rig.dsp.footswitch(plan, &[slot(LIGHT)], true).unwrap();
    let frames = rig.since(mark);
    assert_eq!(frames.len(), 1);
    assert_eq!(
        (frames[0][0], usize::from(frames[0][2])),
        (0x02, at.slot.index())
    );
    assert_eq!(
        (word(&frames[0], 4), word(&frames[0], 16)),
        (0, 1.0_f32.to_bits())
    );
    let edit = LinkedEdit {
        index: 0,
        value: 1.0,
        echo: true,
    };
    assert_eq!(rig.dsp.take_linked_edits(), [(slot(LIGHT), edit)]);
}

/// The controller's note dispatch (`0x514270`): a Minivoicer with MIDI on (15) takes notes
/// on its MIDI CH (0 = channel 1) and drops others; its 20 ms timer sends V1 ACTIVE (5) and
/// V1 NOTE (18).
#[test]
fn minivoicer_takes_notes_on_its_channel() {
    let mut values = vec![0.0; 24];
    values[15] = 1.0;
    let (mut rig, pending) =
        Rig::deploy(|preset, catalog| insert(preset, catalog, LIGHT, 18007, &values));
    assert!(pending.0.is_empty(), "{pending:?}");
    let at = rig.placement(LIGHT);
    let note = |note| DriverMidi::Note {
        note,
        velocity: 100,
        on: true,
    };
    let plan = rig.current.deployment.plan();
    rig.dsp.block_midi(plan, 2, note(60)).unwrap();
    rig.dsp.block_midi(plan, 1, note(62)).unwrap();
    rig.sim.clock.clone().sleep(Duration::from_millis(20));
    let mark = rig.mark();
    rig.dsp.run_timers().unwrap();
    let sent: Vec<(u32, f32)> = rig
        .since(mark)
        .iter()
        .filter(|frame| frame[0] == 0x02 && usize::from(frame[2]) == at.slot.index())
        .map(|frame| (word(frame, 4), f32::from_bits(word(frame, 16))))
        .collect();
    assert_eq!(sent[..2], [(5, 1.0), (18, 62.0 / 127.0)]);
    assert!(!sent.contains(&(18, 60.0 / 127.0)));
}

/// The device stores driver preset writes (`Device::store_linked_edits`): a Dual Delay with
/// LINK FBACK on copies a FEEDBACK L edit to FEEDBACK R (`0x57c3b0` → `0x52382c` →
/// `0x51e4d8`), and a Freeze stomp press writes ON/OFF 1.0 (`0x55acd8`). Each value is in the
/// preset and goes to a connected Cortex Control as a one-value Grid echo (`0x25d51c`).
#[test]
fn driver_preset_writes_are_stored_and_echoed_to_control() {
    use tetrad_app::device::Event;
    use tetrad_app::device::actions::Action;
    use tetrad_control::codecs::ControlMessage;
    use tetrad_hal::hal::Event as HalEvent;
    use tetrad_hal::node::Element;
    use tetrad_settings::controls::FootswitchMode;

    let specs = common::STOCK.specs();
    let catalog = SpecsCatalog(&specs);
    let mut stored = Preset::new_default("4.1.0", &catalog).unwrap();
    let mut delay = vec![ParamValue::Float(0.25); 16];
    delay[14] = ParamValue::Float(1.0);
    let freeze = vec![ParamValue::Float(0.0); 3];
    for (column, block) in [
        (1, Model::block(6012, delay)),
        (2, Model::block(26001, freeze)),
    ] {
        let at = tetrad_preset::Slot { row: 0, column };
        stored.insert_block(at, block, &catalog).unwrap();
    }
    stored
        .stomp_mode_assignments
        .push(tetrad_preset::StompAssignment {
            row: 0,
            column: 2,
            stomp_index: 0,
            ..Default::default()
        });
    let (mut device, log, _media) = common::boot(Media::new([("Linked", stored)]));
    common::connect_control(&mut device);
    let run = |device: &mut tetrad_app::device::Device<Sim>, event| {
        device.push(event);
        (0..10).for_each(|_| _ = device.step());
    };
    let site = |column: u8| Site::Slot(slot((0, column)));
    let stored = |device: &tetrad_app::device::Device<Sim>, column, index| {
        let current = device.state().current.as_ref().unwrap();
        preset::stored_parameter(&current.preset, site(column), index, 0)
    };
    let echoed = |mark, column: u32, index: u32| {
        common::control_sent(&log.borrow(), mark)
            .into_iter()
            .filter_map(|message| match message {
                ControlMessage::Grid(grid) => Some(grid.preset?.bytes),
                _ => None,
            })
            .filter_map(|bytes| Preset::decode(&bytes, tetrad_preset::Limits::default()).ok())
            .filter_map(|preset| {
                let chain = preset.chains.into_iter().next()?;
                let model = chain.models.into_iter().next()?;
                let param = model.params.into_iter().next()?;
                let at = (chain.row, model.column, param.index);
                (at == (Some(0), Some(column), Some(index))).then(|| param.values[0].value.clone())
            })
            .collect::<Vec<_>>()
    };

    let mark = log.borrow().requests.len();
    let edit = Action::SetParameter {
        site: site(1),
        index: 4,
        value: 0.6,
    };
    run(&mut device, Event::Action(edit));
    assert_eq!(stored(&device, 1, 9), Some(0.6));
    assert_eq!(echoed(mark, 1, 9), [Some(ParamValue::Float(0.6))]);

    run(
        &mut device,
        Event::Action(Action::SetMode(FootswitchMode::Stomp)),
    );
    let mark = log.borrow().requests.len();
    let switch_a = Element::all().next().unwrap();
    for value in [1, 0] {
        let stomp = HalEvent::Stomp {
            element: switch_a,
            value,
        };
        run(&mut device, Event::Hal(stomp));
    }
    assert_eq!(stored(&device, 2, 0), Some(1.0));
    assert_eq!(echoed(mark, 2, 0), [Some(ParamValue::Float(1.0))]);
}

/// Minivoicer voice notes (`0x575010` → `0x61d994`): a held note on the block's MIDI channel
/// goes to `State::voice_notes` and draws a frame while the block's editor shows. Each voice
/// that changes goes to Cortex Control as NOTE DISPLAY 23 (`0x4f30c4` → `0x25fb7c`), value
/// `(note + 1) / 128`, 0.0 when the voice falls silent (note −1, review r26 S1); the preset
/// keeps its stored value.
#[test]
fn minivoicer_voice_notes_reach_the_editor_and_cortex_control() {
    use tetrad_app::device::Event;
    use tetrad_app::device::platform::TouchSample;
    use tetrad_app::device::ui::UiInput;
    use tetrad_control::codecs::ControlMessage;
    use tetrad_ui::{Control, GridPosition};

    let mut values = vec![ParamValue::Float(0.0); 25];
    values[15] = ParamValue::Float(1.0);
    let block = Model::block(18007, values);
    let stored = common::block_preset(&common::STOCK.specs(), block, 2);
    let (mut device, log, _media) = common::boot(Media::new([("Voices", stored)]));
    common::connect_control(&mut device);
    let steps =
        |device: &mut tetrad_app::device::Device<Sim>| (0..10).for_each(|_| _ = device.step());
    let cell = Control::SelectCell(GridPosition::new(0, 2).unwrap());
    let at = common::target(&device.bridges().ui, cell);
    for pressed in [true, false] {
        device.push(Event::UiInput(UiInput::Touch(TouchSample {
            pressed,
            ..at
        })));
        steps(&mut device);
    }
    let minivoicer = slot((0, 2));
    assert_eq!(device.state().editor_block, Some(minivoicer));

    let mark = log.borrow().requests.len();
    let mut notes = Vec::new();
    let mut shown = Vec::new();
    for status in [0x90, 0x80] {
        let frames = log.borrow().frames.len();
        let now = device.platform().clock.now();
        for byte in [status, 60, 100] {
            device.push(Event::Midi { byte, at: now });
        }
        // Several 20 ms allocator ticks.
        (0..40).for_each(|_| _ = device.step());
        notes.push(device.state().voice_notes.get(&minivoicer).copied());
        shown.push(device.bridges().ui.view().snapshot().voice_notes);
        assert!(
            log.borrow().frames.len() > frames,
            "the note change is drawn"
        );
    }
    assert_eq!(notes, [Some([Some(60), None]), Some([None; 2])]);
    // The editor read-out shows the same notes (`project.rs`).
    assert_eq!(shown, [[Some(60), None], [None; 2]]);
    let echoes: Vec<(Option<u32>, Option<u32>)> = common::control_sent(&log.borrow(), mark)
        .into_iter()
        .filter_map(|message| match message {
            ControlMessage::Grid(grid) => Some(grid.preset?.bytes),
            _ => None,
        })
        .filter_map(|bytes| Preset::decode(&bytes, tetrad_preset::Limits::default()).ok())
        .filter_map(|preset| {
            let model = preset
                .chains
                .into_iter()
                .next()?
                .models
                .into_iter()
                .next()?;
            let param = model.params.into_iter().next()?;
            let value = match param.values.first()?.value {
                Some(ParamValue::Float(value)) => Some(value.to_bits()),
                _ => None,
            };
            Some((param.index, value))
        })
        .collect();
    assert_eq!(
        echoes,
        [
            (Some(23), Some((61.0_f32 / 128.0).to_bits())),
            (Some(23), Some(0.0_f32.to_bits())),
        ]
    );
    let current = device.state().current.as_ref().unwrap();
    let site = Site::Slot(minivoicer);
    assert_eq!(
        preset::stored_parameter(&current.preset, site, 23, 0),
        Some(0.0)
    );
}

/// The CPU monitor (`0x364ba0`): each DSP Diagnostics reply keeps its core's `process_cpu`
/// (word 5, an `f32`); the total is the mean of the four cores, a silent core counting 0.
#[test]
fn diagnostics_replies_give_the_mean_cpu_load_of_the_four_cores() {
    use tetrad_dsp_protocol::responses::{ResponseType, encode};
    let preset = common::light_preset(&common::STOCK.specs(), 2);
    let (mut device, _log, _media) = common::boot(Media::new([("Light", preset)]));
    assert_eq!(device.state().cpu.total(), None);
    for (core, load) in [(DspCore::Core0, 40.0_f32), (DspCore::Core1, 22.0)] {
        let mut words = [0_u32; 16];
        words[5] = load.to_bits();
        let bytes: Vec<u8> = words.iter().flat_map(|word| word.to_le_bytes()).collect();
        let frame = encode(ResponseType::Diagnostics, core as u8, &bytes);
        let rings = &device.platform().clock.rings;
        common::publish(&mut rings.0.borrow_mut(), CoreId::Dsp(core), &frame);
    }
    (0..10).for_each(|_| _ = device.step());
    assert_eq!(device.state().cpu.total(), Some(15.5));
    assert_eq!(device.state().cpu.percent(), Some(16));
}

/// DSP Diagnostics rows (r16-ui MR 4, `0x364ba0`): a core block gives Drp Msg (word 0), FP exc
/// (word 3), CPU (`f32` words 5, 9, 11), EMDMA (`f32` word 10) and Heap DM/PM/SD (words 6, 8,
/// 7 over 153,592, 114,680 and 2^24); cores without a block stay empty.
#[test]
fn diagnostics_blocks_fill_the_dsp_diagnostics_rows() {
    use tetrad_dsp_protocol::responses::{ResponseType, encode};
    let preset = common::light_preset(&common::STOCK.specs(), 2);
    let (mut device, _log, _media) = common::boot(Media::new([("Light", preset)]));
    let mut words = [0_u32; 16];
    words[0] = 7;
    words[3] = 2;
    [words[5], words[9], words[11], words[10]] = [30.0_f32, 45.0, 12.5, 8.0].map(f32::to_bits);
    [words[6], words[8], words[7]] = [76_796, 28_670, 1 << 23];
    let bytes: Vec<u8> = words.iter().flat_map(|word| word.to_le_bytes()).collect();
    let frame = encode(ResponseType::Diagnostics, DspCore::Core1 as u8, &bytes);
    let rings = &device.platform().clock.rings;
    common::publish(
        &mut rings.0.borrow_mut(),
        CoreId::Dsp(DspCore::Core1),
        &frame,
    );
    (0..10).for_each(|_| _ = device.step());
    let rows = &device.state().diagnostics.cores;
    let row = rows[DspCore::Core1 as usize].expect("core 1 row");
    assert_eq!((row.dropped_messages, row.fp_exceptions), (7, 2));
    assert_eq!((row.cpu, row.emdma), ([30.0, 45.0, 12.5], 8.0));
    assert_eq!(row.heap, [50.0, 25.0, 50.0]);
    assert!(rows[DspCore::Core0 as usize].is_none());
}

/// USB Statistics (`0x689de8` rows, `0x68a90c` values): the SOC2 ARM block by offset, `%d`
/// words, `%llu` counters, byte `0x48`, the HID tx count beside rx word `0x78`, and the
/// read/write distance colours (in: `0x100..=0x200`, out: `0x228..=0x328`).
#[test]
fn usb_statistics_decode_the_soc2_arm_block() {
    use tetrad_app::device::info::{DspDiagnostics, UsbTone};
    use tetrad_dsp_protocol::responses::Soc2ArmDiagnostics;
    let mut record = [0_u8; 128];
    let mut put = |at: usize, bytes: &[u8]| record[at..at + bytes.len()].copy_from_slice(bytes);
    put(0x00, &[1]);
    put(0x04, &3_u32.to_le_bytes());
    put(0x08, &4_u32.to_le_bytes());
    put(0x30, &0x400_u32.to_le_bytes());
    put(0x34, &0x180_u32.to_le_bytes());
    put(0x48, &[5]);
    put(0x58, &(1_u64 << 33).to_le_bytes());
    put(0x60, &9_u64.to_le_bytes());
    put(0x78, &(-2_i32).to_le_bytes());
    let mut diagnostics = DspDiagnostics::default();
    assert!(
        diagnostics.usb_statistics(7).is_none(),
        "\"---\" before a block"
    );
    diagnostics.soc2 = Some(Soc2ArmDiagnostics(record));
    let rows = diagnostics.usb_statistics(7).unwrap();
    let value = |index: usize| (rows[index].label, rows[index].value.as_str());
    assert_eq!(value(0), ("USBConnected", "Yes"));
    assert_eq!(value(1), ("IN rd/wr idx", "3/4"));
    assert_eq!(value(3), ("UI to DSP HID count (tx/rx)", "7/-2"));
    assert_eq!((value(4).1, rows[4].tone), ("384", UsbTone::InRange));
    assert_eq!((value(5).1, rows[5].tone), ("1024", UsbTone::OutOfRange));
    assert_eq!(value(12), ("USBAudioOUTBufferResets", "5"));
    assert_eq!(value(17), ("USBMidiInCount", "9"));
    assert_eq!(value(18), ("USBMidiOutCount", "8589934592"));
    assert_eq!(rows[13].tone, UsbTone::Plus);
}

/// Row meters (r16-perform MR 3, `0x4df0f4`): while Cortex Control subscribes grid meters, or
/// the Grid shows an Input Gate meter (`0x5a36d0`: row 1 reads In 1 with its gate on), each
/// poll sends `MeterRequest` slot 1 for `0x401c..=0x401f` to the gate core. A column-1 reply
/// is kept per row for the Control sender, which sends it only while the row's Input Gate is
/// on (`0x110194`). With every gate bypassed, the poll stops after DELETE All.
#[test]
fn row_meters_poll_the_gate_core_while_control_subscribes() {
    use tetrad_control::codecs::{ControlMessage, GridModelMeterMessage, MessageAction};
    use tetrad_dsp_protocol::responses::{ResponseType, encode};
    let preset = common::light_preset(&common::STOCK.specs(), 2);
    let (mut device, log, _media) = common::boot(Media::new([("Light", preset)]));
    let gate = (device.state().current.as_ref())
        .and_then(|current| current.deployment.plan().gate_core)
        .expect("the preset has an input gate");
    let row_polls = |log: &common::Log, mark: usize| {
        (log.requests[mark..].iter())
            .filter(|sent| sent.kind() == 0x0e && sent.frame[2] == 1)
            .map(|sent| u32::from_le_bytes(sent.frame[4..8].try_into().unwrap()))
            .collect::<Vec<_>>()
    };
    let mark = log.borrow().requests.len();
    (0..200).for_each(|_| _ = device.step());
    assert!(
        row_polls(&log.borrow(), mark).contains(&0x401c),
        "the Grid gate meter"
    );
    common::connect_control(&mut device);
    let meter = |action: MessageAction, row| GridModelMeterMessage {
        action: Some(action.into()),
        row: Some(row),
        column: Some(-1),
        ..Default::default()
    };
    let mark = log.borrow().requests.len();
    common::host_send(&mut device, meter(MessageAction::Create, 1));
    (0..200).for_each(|_| _ = device.step());
    let polls = row_polls(&log.borrow(), mark);
    assert_eq!(polls[..4], [0x401c, 0x401d, 0x401e, 0x401f]);
    let core = DspCore::try_from(gate.index() as u8).unwrap();
    let words = [core as u32, 1, 0x401d, (-6.5_f32).to_bits()];
    let bytes: Vec<u8> = words.iter().flat_map(|word| word.to_le_bytes()).collect();
    let frame = encode(ResponseType::Meter, core as u8, &bytes);
    let rings = &device.platform().clock.rings;
    common::publish(&mut rings.0.borrow_mut(), CoreId::Dsp(core), &frame);
    (0..10).for_each(|_| _ = device.step());
    assert_eq!(device.state().row_meters, [None, Some(-6.5), None, None]);
    // `0x110194`: a row meter goes to Control only while the row's Input Gate is on.
    let metered = |log: &common::Log, mark: usize| {
        (common::control_sent(log, mark).into_iter())
            .filter_map(|message| match message {
                ControlMessage::Grid(grid) => grid.preset,
                _ => None,
            })
            .filter_map(|body| tetrad_preset::Preset::decode(&body.bytes, Default::default()).ok())
            .flat_map(|body| body.chains)
            .filter(|chain| !chain.input_control.is_empty())
            .count()
    };
    let run = |device: &mut tetrad_app::device::Device<common::Sim>, millis: u64| {
        let until = device.platform().clock.now() + Duration::from_millis(millis);
        while device.platform().clock.now() < until {
            device.step();
        }
    };
    let mark = log.borrow().requests.len();
    run(&mut device, 500);
    assert!(metered(&log.borrow(), mark) > 0, "the row meter goes out");
    let bypassed = |row| tetrad_preset::Chain {
        row: Some(row),
        input_control: vec![tetrad_preset::Model {
            params: vec![tetrad_preset::Param {
                index: Some(1),
                values: vec![tetrad_preset::SceneValue {
                    value: Some(tetrad_preset::ParamValue::Float(1.0)),
                    ..Default::default()
                }],
                ..Default::default()
            }],
            ..Default::default()
        }],
        ..Default::default()
    };
    // A Grid UPDATE edits its first chain only (`0x25657c`).
    for row in [0, 1] {
        let bypass = tetrad_preset::Preset {
            chains: vec![bypassed(row)],
            ..Default::default()
        };
        common::host_send(
            &mut device,
            tetrad_control::codecs::GridMessage {
                action: Some(MessageAction::Update.into()),
                preset: Some(tetrad_control::codecs::RawMessageBytes {
                    bytes: bypass.to_bytes(),
                }),
                ..Default::default()
            },
        );
    }
    run(&mut device, 200);
    let mark = log.borrow().requests.len();
    run(&mut device, 500);
    assert_eq!(
        metered(&log.borrow(), mark),
        0,
        "a bypassed gate has no row meter"
    );
    common::host_send(&mut device, meter(MessageAction::Delete, -1));
    (0..50).for_each(|_| _ = device.step());
    let mark = log.borrow().requests.len();
    (0..200).for_each(|_| _ = device.step());
    assert!(row_polls(&log.borrow(), mark).is_empty());
}

/// Output calibration gains (`0x36d734`): each `CAL_*` U-Boot value in dB, limited to −4..=0
/// dB, goes out as `10^(dB/20)` on its output; missing values take the stock defaults (−2 dB,
/// HP 0 dB) and text that is not a number reads 0 dB.
#[test]
fn output_calibration_gains_follow_the_uboot_values() {
    let env: std::collections::BTreeMap<String, String> = [
        ("CAL_OUT_1", "-1"),
        ("CAL_SEND_1", "-9"),
        ("CAL_HP_R", "junk"),
    ]
    .into_iter()
    .map(|(name, value)| (name.to_owned(), value.to_owned()))
    .collect();
    let gains: Vec<(u8, f32)> = dsp::output_calibration(&env)
        .map(|request| match request {
            Request::SetOutputCalibrationGains { output, gain_bits } => {
                (output, f32::from_bits(gain_bits))
            }
            other => panic!("unexpected {other:?}"),
        })
        .collect();
    let db = |db: f32| 10_f32.powf(db / 20.0);
    let expected = [-1.0, -2.0, -2.0, -2.0, -4.0, -2.0, 0.0, 0.0];
    assert_eq!(gains.len(), 8);
    for ((output, gain), (index, want)) in gains.into_iter().zip((0u8..).zip(expected)) {
        assert_eq!(output, index);
        assert!((gain - db(want)).abs() < 1e-6, "output {index}: {gain}");
    }
}

/// Model meters (`0x4a9844`, `0x4a9a64`, `0x25da6c`): while Control subscribes a block, each
/// poll sends one `MeterRequest` per meter parameter of its model to the block's core and slot
/// (word = the parameter index). A linear reply becomes `1 − |dB / (max − min)|` and goes out
/// in the Grid meter UPDATE as a model with its column and the parameter.
#[test]
fn model_meters_poll_subscribed_blocks_and_reach_the_control_grid_update() {
    use tetrad_control::codecs::{ControlMessage, GridModelMeterMessage, MessageAction};
    use tetrad_dsp_protocol::responses::{ResponseType, encode};
    use tetrad_preset::{Model, ParamValue};
    let block = Model::block(5026, [0.0; 3].map(ParamValue::Float));
    let preset = common::block_preset(&common::STOCK.specs(), block, 3);
    let (mut device, log, _media) = common::boot(Media::new([("Comp", preset)]));
    let placement = (device.state().current.as_ref())
        .and_then(|current| {
            let slot = tetrad_engine::Slot {
                row: tetrad_engine::Row::new(0).unwrap(),
                column: tetrad_engine::Column::new(3).unwrap(),
            };
            current.deployment.plan().block(slot)
        })
        .expect("the block is deployed");
    let (core, slot) = (placement.core.index() as u8, placement.slot.index() as u8);
    common::connect_control(&mut device);
    let mark = log.borrow().requests.len();
    common::host_send(
        &mut device,
        GridModelMeterMessage {
            action: Some(MessageAction::Create.into()),
            row: Some(0),
            column: Some(3),
            ..Default::default()
        },
    );
    (0..200).for_each(|_| _ = device.step());
    let polls: Vec<(u8, u8, u32)> = (log.borrow().requests[mark..].iter())
        .filter(|sent| sent.kind() == 0x0e)
        .map(|sent| {
            let word = u32::from_le_bytes(sent.frame[4..8].try_into().unwrap());
            (sent.frame[1], sent.frame[2], word)
        })
        .collect();
    assert!(polls.contains(&(core, slot, 2)), "{polls:?}");

    let words = [u32::from(core), u32::from(slot), 2, 0.5_f32.to_bits()];
    let bytes: Vec<u8> = words.iter().flat_map(|word| word.to_le_bytes()).collect();
    let core = DspCore::try_from(core).unwrap();
    let frame = encode(ResponseType::Meter, core as u8, &bytes);
    let rings = &device.platform().clock.rings;
    common::publish(&mut rings.0.borrow_mut(), CoreId::Dsp(core), &frame);
    let mark = log.borrow().requests.len();
    (0..200).for_each(|_| _ = device.step());
    let expected = 1.0 - (20.0 * 0.5_f32.log10() / 50.0).abs();
    let sent = common::control_sent(&log.borrow(), mark);
    let model = (sent.iter())
        .find_map(|message| match message {
            ControlMessage::Grid(grid) => {
                let bytes = &grid.preset.as_ref()?.bytes;
                let body =
                    tetrad_preset::Preset::decode(bytes, tetrad_preset::Limits::default()).ok()?;
                let chain = body.chains.into_iter().find(|chain| chain.row == Some(0))?;
                chain.models.into_iter().next()
            }
            _ => None,
        })
        .expect("a Grid meter update with the model");
    assert_eq!(model.column, Some(3));
    let [param] = &model.params[..] else {
        panic!("one meter parameter: {:?}", model.params);
    };
    assert_eq!((param.index, param.is_led), (Some(2), None));
    let Some(ParamValue::Float(value)) = param.values[0].value else {
        panic!("a float meter value: {param:?}");
    };
    assert!((value - expected).abs() < 1e-6, "{value} != {expected}");
}

/// A3: a layout edit redeploys the whole layout, as stock `Chain::dropSlot` (`0x59f620` →
/// `0x5122c0`) does, but reads no blob file: stock imports every blob once at boot
/// (`0x880880`, map `+0x1bbc`) and takes the per-core variant from that map (`0x87b228`). The
/// QC drop took 247 ms in `DeployLayout`.
#[test]
fn a_layout_edit_redeploys_without_reading_blob_files_again() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tetrad_app::device::Event;
    use tetrad_app::device::actions::{Action, LayoutEdit};
    static READS: AtomicUsize = AtomicUsize::new(0);
    let fixture = Fixture {
        blob: |name| {
            READS.fetch_add(1, Ordering::Relaxed);
            (common::STOCK.blob)(name)
        },
        ..common::STOCK
    };
    let media = Media::new([("Light", common::light_preset(&fixture.specs(), 2))]);
    let log = Rc::new(RefCell::new(Log::default()));
    let mut device = tetrad_app::device::Device::start(Sim::new(fixture, &media, &log)).unwrap();
    let booted = READS.load(Ordering::Relaxed);
    assert!(booted > 0, "the boot deploy reads its blob");
    let slot = |column| Slot {
        row: Row::new(0).unwrap(),
        column: Column::new(column).unwrap(),
    };
    for (from, to) in [(slot(2), slot(5)), (slot(5), slot(1))] {
        let mark = log.borrow().requests.len();
        let edit = Action::EditLayout(LayoutEdit::Move { from, to });
        device.push(Event::Action(edit));
        (0..5).for_each(|_| _ = device.step());
        assert!(log.borrow().kinds(mark).contains(&0x1e), "a redeploy");
        assert_eq!(READS.load(Ordering::Relaxed), booted);
    }
}

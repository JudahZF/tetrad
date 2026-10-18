//! Hybrid V1 capture session over synthetic backends: no stock stimulus or model bytes.

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use tetrad_capture::entry::{CapturePlacement, EntryAction, NcPreset};
use tetrad_capture::orchestration::{
    BufferId, CallerFact, Command, Gate, NcError, NcState, Notice, Operation, ParamWrite,
    StockError,
};
use tetrad_capture::session::{
    CaptureDsp, CaptureLibrary, CapturePaths, CaptureRecord, CaptureServices, CaptureStorage,
    DspError, Entered, Input, MeterRequest, MeterSource, Output, RemoteUpdate, SaveInfo,
    ServiceError, Session, SessionError, SharcMemory, TrainerSeed,
};
use tetrad_dsp_protocol::{DeploymentTarget, DspCore, ModuleSlot};

const INPUT_BYTES: usize = 64;
const REFINE_BYTES: usize = 96;

#[derive(Default)]
struct Dsp {
    allocated: Vec<(BufferId, u32)>,
    freed: Vec<BufferId>,
    params: Vec<ParamWrite>,
    meters: Vec<MeterRequest>,
    /// `(mix, level, parameter writes sent before it)`.
    mixes: Vec<(u8, f32, usize)>,
    placed: Vec<DeploymentTarget>,
}

impl CaptureDsp for Dsp {
    fn allocate(&mut self, id: BufferId, bytes: u32) -> Result<(), DspError> {
        self.allocated.push((id, bytes));
        Ok(())
    }
    fn free(&mut self, id: BufferId) -> Result<(), DspError> {
        self.freed.push(id);
        Ok(())
    }
    fn set_parameter(&mut self, write: ParamWrite) -> Result<(), DspError> {
        self.params.push(write);
        Ok(())
    }
    fn request_meter(&mut self, request: MeterRequest) -> Result<(), DspError> {
        self.meters.push(request);
        Ok(())
    }
    fn set_placed_parameter(
        &mut self,
        target: DeploymentTarget,
        write: ParamWrite,
    ) -> Result<(), DspError> {
        self.placed.push(target);
        self.set_parameter(write)
    }
    fn set_headphone_mix(&mut self, mix: u8, level: f32) -> Result<(), DspError> {
        self.mixes.push((mix, level, self.params.len()));
        Ok(())
    }
}

#[derive(Default)]
struct Memory {
    blocks: HashMap<u32, Vec<u8>>,
    fail_writes: bool,
}

impl SharcMemory for Memory {
    fn write(&mut self, address: u32, bytes: &[u8]) -> io::Result<()> {
        if self.fail_writes {
            return Err(io::Error::other("synthetic mmap failure"));
        }
        self.blocks.insert(address, bytes.to_vec());
        Ok(())
    }
    fn read(&mut self, address: u32, out: &mut [u8]) -> io::Result<()> {
        let block = self.blocks.get(&address).map_or(&[][..], Vec::as_slice);
        out.fill(0);
        let n = block.len().min(out.len());
        out[..n].copy_from_slice(&block[..n]);
        Ok(())
    }
}

#[derive(Default)]
struct Storage(HashMap<PathBuf, Vec<u8>>);

impl CaptureStorage for Storage {
    fn file_len(&mut self, path: &Path) -> io::Result<u64> {
        self.0
            .get(path)
            .map(|file| file.len() as u64)
            .ok_or_else(|| io::ErrorKind::NotFound.into())
    }
    fn read(&mut self, path: &Path, out: &mut [u8]) -> io::Result<()> {
        let file = self.0.get(path).ok_or(io::ErrorKind::NotFound)?;
        out.copy_from_slice(file.get(..out.len()).ok_or(io::ErrorKind::UnexpectedEof)?);
        Ok(())
    }
    fn write(&mut self, path: &Path, bytes: &[u8]) -> io::Result<()> {
        self.0.insert(path.to_path_buf(), bytes.to_vec());
        Ok(())
    }
}

fn target(core: DspCore, slot: u32) -> DeploymentTarget {
    DeploymentTarget {
        core,
        slot: ModuleSlot::try_from(slot).unwrap(),
    }
}

/// Synthetic deployments: node 1 on core 0 slot 5 (recorder) or 6 (refiner); the
/// conductor (node 0) on core 0 slot 2 and the node trainer (node 4) on core 1 slot 3.
fn deployed(preset: NcPreset, node: u8) -> Option<DeploymentTarget> {
    match (preset, node) {
        (NcPreset::Recorder, 1) => Some(target(DspCore::Core0, 5)),
        (NcPreset::Refiner, 1) => Some(target(DspCore::Core0, 6)),
        (NcPreset::Trainer, 0) => Some(target(DspCore::Core0, 2)),
        (NcPreset::Trainer, 4) => Some(target(DspCore::Core1, 3)),
        _ => None,
    }
}

/// Runs every DSPController plan and quiescence at once; discard waits for the user.
#[derive(Default)]
struct Services {
    gates: Vec<Gate>,
    plans: Vec<Vec<EntryAction>>,
}

impl CaptureServices for Services {
    fn enter(&mut self, _state: NcState, plan: &[EntryAction]) -> Result<Entered, ServiceError> {
        self.plans.push(plan.to_vec());
        let preset = plan.iter().find_map(|action| match action {
            EntryAction::Activate(preset) => Some(*preset),
            _ => None,
        });
        Ok(Entered::Done(preset.map(|preset| {
            CapturePlacement::new(
                (0..8).filter_map(|node| Some(((0, node), deployed(preset, node)?))),
            )
        })))
    }
    fn request(&mut self, gate: Gate) -> Result<Option<CallerFact>, ServiceError> {
        self.gates.push(gate);
        Ok(match gate {
            Gate::Quiesce => Some(CallerFact::BackendQuiesced),
            Gate::DiscardDecision => None,
            other => panic!("the session resolves {other:?} itself"),
        })
    }
}

/// The library already holds `known`; inserted records are kept.
#[derive(Default)]
struct Library {
    known: Option<String>,
    fail_inserts: bool,
    inserted: Vec<(String, Vec<u8>)>,
    updated: Vec<(String, SaveInfo)>,
}

impl CaptureLibrary for Library {
    fn contains(&mut self, hash: &str) -> io::Result<bool> {
        Ok(self.known.as_deref() == Some(hash))
    }
    fn insert(&mut self, record: &CaptureRecord<'_>) -> io::Result<()> {
        if self.fail_inserts {
            return Err(io::Error::other("synthetic library failure"));
        }
        self.inserted
            .push((record.hash.clone(), record.model.to_vec()));
        Ok(())
    }
    fn update(&mut self, hash: &str, info: &SaveInfo) -> io::Result<()> {
        self.updated.push((hash.to_owned(), info.clone()));
        Ok(())
    }
}

type TestSession = Session<Dsp, Memory, Storage, Services>;

fn address(id: BufferId) -> u32 {
    0x2000_0000 + (id as u32) * 0x10_0000
}

fn session(memory: Memory) -> TestSession {
    let paths = CapturePaths::stock();
    let mut storage = Storage::default();
    storage
        .0
        .insert(paths.input_stimulus.clone(), vec![0x11; INPUT_BYTES]);
    storage
        .0
        .insert(paths.refine_stimulus.clone(), vec![0x22; REFINE_BYTES]);
    // Seed: first mt19937 output for random_device 5489 (VA 0x33b02c).
    let seed = TrainerSeed::from_random_device(5489);
    Session::new(
        paths,
        Dsp::default(),
        memory,
        storage,
        Services::default(),
        seed,
    )
    .unwrap()
}

/// Handles `input`, then plays a DSP that answers model starts with raw `(core, slot)`
/// replies of the active capture preset, and a DSPController that unmutes on request.
fn send(session: &mut TestSession, input: Input) -> Vec<Output> {
    let mut outputs = session.handle(input).unwrap();
    loop {
        let preset = session.controller().active().unwrap_or(NcPreset::Recorder);
        let start = |node| Input::ModelStart {
            target: deployed(preset, node).unwrap(),
            ok: true,
        };
        let signals: Vec<Input> = session
            .machine()
            .pending_requests()
            .flat_map(|(_, operation, _)| match operation {
                Operation::AwaitCaller(Gate::PlayerStarted) => vec![start(1)],
                Operation::AwaitCaller(Gate::TrainersStarted) => vec![start(0), start(4)],
                Operation::AwaitCaller(Gate::Unmuted) => vec![Input::Unmuted],
                _ => Vec::new(),
            })
            .collect();
        if signals.is_empty() {
            return outputs;
        }
        for signal in signals {
            outputs.extend(session.handle(signal).unwrap());
        }
    }
}

/// Answers every allocation the DSP has not yet seen a reply for.
fn allocate(session: &mut TestSession, answered: &mut usize) -> Vec<Output> {
    let mut outputs = Vec::new();
    while let Some(&(id, bytes)) = session.dsp().allocated.get(*answered) {
        *answered += 1;
        let address = address(id);
        outputs.extend(send(session, Input::BufferAllocated { id, bytes, address }));
    }
    outputs
}

fn meter(session: &mut TestSession, core: u8, meter: u16, value: f32) -> Vec<Output> {
    session.poll_meters().unwrap();
    send(session, Input::Meter { core, meter, value })
}

fn next(session: &mut TestSession) {
    send(session, Input::Command(Command::Next));
}

fn synthetic_export() -> Vec<u8> {
    let mut bytes = vec![0; 0x2864];
    bytes[0..4].copy_from_slice(&48_000.0f32.to_le_bytes());
    bytes[4..8].copy_from_slice(&0x200u32.to_le_bytes());
    bytes[8..12].copy_from_slice(&0x800u32.to_le_bytes());
    bytes
}

fn write(node: u8, parameter: u8, value: f32) -> ParamWrite {
    ParamWrite {
        node,
        parameter,
        value,
    }
}

/// Drives one session from Open to Training through the stock meter routing.
fn to_training(session: &mut TestSession, answered: &mut usize) {
    send(session, Input::Command(Command::Open));
    for _ in 0..6 {
        next(session);
    }
    assert_eq!(session.machine().state(), NcState::Calibrate);
    allocate(session, answered);
    meter(session, 0, 4, 5.0);
    assert_eq!(session.machine().state(), NcState::RefineCapture);
    // Delay request: meter 5 at node 1 (VA 0x339320), sent to its recorder slot
    // (VA 0x510ba0) and answered by the next meter-5 reply.
    assert!(session.dsp().meters.contains(&MeterRequest {
        core: 0,
        source: MeterSource::Column(5),
        meter: 5
    }));
    send(
        session,
        Input::Meter {
            core: 0,
            meter: 5,
            value: 37.0,
        },
    );
    meter(session, 0, 4, 9.9);
    assert_eq!(session.machine().state(), NcState::Capturing);
    allocate(session, answered);
    meter(session, 0, 4, 30.0);
    send(
        session,
        Input::Meter {
            core: 0,
            meter: 6,
            value: 0.0,
        },
    );
    assert_eq!(session.machine().state(), NcState::Training);
}

#[test]
fn hybrid_flow_moves_stimuli_captures_and_export_through_backends() {
    let mut memory = Memory::default();
    memory
        .blocks
        .insert(address(BufferId::RefinerExport), synthetic_export());
    let mut session = session(memory);
    let mut answered = 0;
    to_training(&mut session, &mut answered);

    // Stimuli reached their SHARC buffers; the refiner and target captures reached files.
    let memory = &session.memory().blocks;
    assert_eq!(memory[&address(BufferId::Input)], vec![0x11; INPUT_BYTES]);
    let paths = session.paths().clone();
    assert_eq!(
        session.storage().0[&paths.refiner_capture].len(),
        REFINE_BYTES
    );
    assert_eq!(session.storage().0[&paths.input_capture].len(), INPUT_BYTES);
    let params = &session.dsp().params;
    assert!(params.contains(&write(1, 0, (REFINE_BYTES / 4) as f32)));
    assert!(params.contains(&write(1, 0, (INPUT_BYTES / 4) as f32)));
    let seed = 3_499_211_612u32 as f32;
    assert_eq!(
        params[params.len() - 4..],
        [
            write(0, 4, seed),
            write(4, 4, seed),
            write(0, 0, 1.0),
            write(4, 0, 1.0)
        ]
    );
    // Trainer writes reach the deployed conductor and node trainer (VA 0x510ba0).
    let placed = &session.dsp().placed;
    let (conductor, node) = (target(DspCore::Core0, 2), target(DspCore::Core1, 3));
    assert_eq!(
        placed[placed.len() - 4..],
        [conductor, node, conductor, node]
    );

    // State-1 tick: headphone mixes 1 = 1.0 and 8 = 0.0 before any node write (VA 0x339320).
    assert_eq!(session.dsp().mixes, [(1, 1.0, 0), (8, 0.0, 0)]);

    // Training: meter 5 is progress and meter 6 is loss (VA 0x3cbbd8). The node trainer
    // is polled on core 1 at its own slot (VA 0x3cbedc).
    let outputs = meter(&mut session, 0, 5, 42.4);
    assert!(session.dsp().meters.ends_with(&[
        MeterRequest {
            core: 0,
            source: MeterSource::Column(2),
            meter: 7
        },
        MeterRequest {
            core: 1,
            source: MeterSource::Column(3),
            meter: 7
        }
    ]));
    assert!(outputs.contains(&Output::Remote(RemoteUpdate::Progress(42))));
    meter(&mut session, 0, 6, 0.02);
    meter(&mut session, 0, 5, 90.0);
    assert_eq!(session.machine().state(), NcState::AutoRefine);
    allocate(&mut session, &mut answered);
    let memory = &session.memory().blocks;
    assert_eq!(
        memory[&address(BufferId::RefinerCapture)].len(),
        REFINE_BYTES
    );
    assert_eq!(
        session.dsp().params[session.dsp().params.len() - 3..],
        [write(1, 4, 1.0), write(1, 2, 37.0), write(1, 0, 1.0)]
    );

    // Refinement complete, then the scheduled export reads buffer 5 once the counter moves.
    meter(&mut session, 0, 7, 100.0);
    assert_eq!(session.machine().state(), NcState::Exporting);
    meter(&mut session, 0, 8, 1.0);
    assert_eq!(session.machine().state(), NcState::RefineReady);
    assert_eq!(session.machine().model(), Some(&synthetic_export()[..]));
    assert_eq!(session.dsp().params.last(), Some(&write(1, 6, 1.0)));
    assert!(session.machine().error().is_none());
}

/// DSPController plans run on every state change (VA 0x528498) and answer the entry
/// gates. MicSetup's unmute holds through Calibrate: stock latch `+0x18` (VA 0x338118).
#[test]
fn entry_plans_answer_gates_and_the_unmute_latch_spans_states() {
    let mut session = session(Memory::default());
    session.handle(Input::Command(Command::Open)).unwrap();
    let plans = &session.services().plans;
    assert_eq!(plans.len(), 1);
    assert!(plans[0].contains(&EntryAction::Activate(NcPreset::Recorder)));
    assert_eq!(plans[0].last(), Some(&EntryAction::InputGainOverride(true)));
    // Raw replies for other sites change nothing; the player reply completes state 1.
    let conductor = Input::ModelStart {
        target: target(DspCore::Core0, 2),
        ok: true,
    };
    session.handle(conductor).unwrap();
    assert!(session.dsp().params.is_empty());
    let player = Input::ModelStart {
        target: deployed(NcPreset::Recorder, 1).unwrap(),
        ok: true,
    };
    session.handle(player).unwrap();
    assert_eq!(session.dsp().params, [write(1, 2, 1.0), write(1, 2, 1.0)]);
    for _ in 0..6 {
        session.handle(Input::Command(Command::Next)).unwrap();
    }
    assert_eq!(session.machine().state(), NcState::Calibrate);
    assert_eq!(
        session.services().plans.last().unwrap()[..],
        [EntryAction::StoreCab]
    );
    // No unmute signal was sent, yet Calibrate does not wait for one.
    assert!(
        !session
            .machine()
            .pending_requests()
            .any(|(_, op, _)| *op == Operation::AwaitCaller(Gate::Unmuted))
    );
    assert_eq!(session.dsp().allocated.len(), 2);
}

fn refine_ready() -> TestSession {
    let mut memory = Memory::default();
    memory
        .blocks
        .insert(address(BufferId::RefinerExport), synthetic_export());
    let mut session = session(memory);
    let mut answered = 0;
    to_training(&mut session, &mut answered);
    meter(&mut session, 0, 5, 90.0);
    allocate(&mut session, &mut answered);
    meter(&mut session, 0, 7, 100.0);
    meter(&mut session, 0, 8, 1.0);
    assert_eq!(session.machine().state(), NcState::RefineReady);
    session
}

#[test]
fn save_keys_the_raw_export_by_sha256_and_restarts_only_for_new_captures() {
    // Independent hashlib value for the synthetic export.
    let hash = "da6d64894620eda60cc991789276a9e8851007ba5a2f5c72067b9be12b8ec37b";
    let info = SaveInfo {
        name: "Plexi 2".to_owned(),
        device: 1,
        instrument: 0,
    };

    // A failed library write leaves no remote output for the next call (review L8).
    let mut session = refine_ready();
    let mut failing = Library {
        fail_inserts: true,
        ..Library::default()
    };
    assert!(matches!(
        session.save(info.clone(), &mut failing),
        Err(SessionError::Library(_))
    ));
    assert_eq!(session.machine().state(), NcState::RefineReady);

    // A known hash only updates the record; the run stays ready (VA 0x3d3f18).
    let mut library = Library {
        known: Some(hash.to_owned()),
        ..Library::default()
    };
    assert_eq!(
        session.save(info.clone(), &mut library).unwrap(),
        [
            Output::Remote(RemoteUpdate::SaveInfo(info.clone())),
            Output::Saved {
                hash: hash.to_owned(),
                new: false
            }
        ]
    );
    assert_eq!(library.updated, [(hash.to_owned(), info.clone())]);
    assert!(library.inserted.is_empty());
    assert_eq!(session.machine().state(), NcState::RefineReady);

    // A new hash inserts the raw export, frees every buffer and restarts at MicSetup.
    let mut library = Library::default();
    let outputs = session.save(info.clone(), &mut library).unwrap();
    assert_eq!(library.inserted, [(hash.to_owned(), synthetic_export())]);
    assert_eq!(
        outputs.first(),
        Some(&Output::Remote(RemoteUpdate::SaveInfo(info)))
    );
    assert!(outputs.contains(&Output::Saved {
        hash: hash.to_owned(),
        new: true
    }));
    assert_eq!(session.machine().state(), NcState::MicSetup);
    assert!(session.machine().model().is_none());
    // MicSetup restores the state-1 setup: player start, then node1 p2 = 1 twice.
    let player = deployed(NcPreset::Recorder, 1).unwrap();
    send(
        &mut session,
        Input::ModelStart {
            target: player,
            ok: true,
        },
    );
    assert!(session.machine().cleanup_complete());
    assert_eq!(session.dsp().params.last(), Some(&write(1, 2, 1.0)));
}

#[test]
fn memory_failure_enters_handle_error_and_frees_every_buffer() {
    let mut session = session(Memory {
        fail_writes: true,
        ..Memory::default()
    });
    let mut answered = 0;
    send(&mut session, Input::Command(Command::Open));
    for _ in 0..6 {
        next(&mut session);
    }
    let outputs = allocate(&mut session, &mut answered);
    let error = NcError::Stock(StockError::MemoryWrite);
    assert!(outputs.contains(&Output::Notice(Notice::Error(error))));
    assert_eq!(session.machine().state(), NcState::HandleError);
    assert!(session.machine().cleanup_complete());
    let mut freed = session.dsp().freed.clone();
    freed.sort_by_key(|id| *id as u8);
    assert_eq!(freed, [BufferId::RefinerStimulus, BufferId::RefinerCapture]);
    assert!(session.services().gates.contains(&Gate::Quiesce));
}

#[test]
fn stale_meters_are_ignored_and_both_disengaged_trainers_are_reported() {
    let mut session = session(Memory::default());
    let mut answered = 0;
    to_training(&mut session, &mut answered);
    // The last poll was in Capturing: its epoch cannot complete training.
    let outputs = session
        .handle(Input::Meter {
            core: 0,
            meter: 5,
            value: 90.0,
        })
        .unwrap();
    assert!(outputs.is_empty());
    assert_eq!(session.machine().state(), NcState::Training);
    assert!(meter(&mut session, 0, 7, 0.4).is_empty());
    assert!(meter(&mut session, 0, 7, 0.5).is_empty());
    assert_eq!(meter(&mut session, 1, 7, 1.0), [Output::TrainersDisengaged]);
    // The app answers with Command::Close, as the stock dialog close does (VA 0x3c9a04).
    send(&mut session, Input::Command(Command::Close));
    assert_eq!(session.machine().state(), NcState::Inactive);
}

#[test]
fn ab_toggle_in_refine_ready_writes_node1_p3_and_reports_remote() {
    let mut session = refine_ready();
    let outputs = session
        .handle(Input::Command(Command::ToggleAb(false)))
        .unwrap();
    assert!(outputs.contains(&Output::Remote(RemoteUpdate::ToggleAbModel(false))));
    assert_eq!(session.dsp().params.last(), Some(&write(1, 3, 1.0)));
    session
        .handle(Input::Command(Command::ToggleAb(true)))
        .unwrap();
    assert_eq!(session.dsp().params.last(), Some(&write(1, 3, 0.0)));
}

/// Corpus check: the firmware stimuli fit the session bounds (sizes only, no bytes read).
#[test]
#[ignore = "reads $QC_CORPUS_DIR/rootfs/opt/neuraldsp/sharc_binaries"]
fn stock_stimulus_sizes_fit_the_session() {
    let Some(corpus) = std::env::var_os("QC_CORPUS_DIR") else {
        eprintln!("skipped: set QC_CORPUS_DIR");
        return;
    };
    let dir = Path::new(&corpus).join("rootfs/opt/neuraldsp/sharc_binaries");
    let paths = CapturePaths {
        input_stimulus: dir.join("input.bin"),
        refine_stimulus: dir.join("refine.bin"),
        ..CapturePaths::stock()
    };
    let session = Session::new(
        paths,
        Dsp::default(),
        Memory::default(),
        tetrad_capture::session::FsStorage,
        Services::default(),
        TrainerSeed::from_random_device(5489),
    )
    .unwrap();
    assert_eq!(session.machine().state(), NcState::Inactive);
}

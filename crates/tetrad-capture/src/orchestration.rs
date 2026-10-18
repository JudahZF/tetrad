//! Inert, bounded V1 capture orchestration from ZenUI VA 0x339320.
//!
//! This is control flow, not a trainer, routing service or physical-output proof.

const REQUEST_LIMIT: usize = 48;
const PARAMETER_BYTES: u32 = 0x283c;
const COMMS_BYTES: u32 = 0x10;
pub const EXPORT_BYTES: u32 = 0x2864;
const MAX_STIMULUS_BYTES: u32 = 16 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum NcState {
    Inactive = 0,
    Initial = 1,
    DiagramStep1 = 2,
    DiagramStep2 = 3,
    DiagramStep3 = 4,
    DiagramStep4 = 5,
    MicSetup = 6,
    Calibrate = 7,
    RefineCapture = 8,
    Capturing = 9,
    SanityCheck = 10,
    Training = 11,
    AutoRefine = 12,
    RefineReady = 13,
    ManualRefine = 14,
    Exporting = 15,
    HandleError = 16,
}

impl TryFrom<u8> for NcState {
    type Error = Refusal;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        let states = [
            Self::Inactive,
            Self::Initial,
            Self::DiagramStep1,
            Self::DiagramStep2,
            Self::DiagramStep3,
            Self::DiagramStep4,
            Self::MicSetup,
            Self::Calibrate,
            Self::RefineCapture,
            Self::Capturing,
            Self::SanityCheck,
            Self::Training,
            Self::AutoRefine,
            Self::RefineReady,
            Self::ManualRefine,
            Self::Exporting,
            Self::HandleError,
        ];
        states
            .get(usize::from(value))
            .copied()
            .ok_or(Refusal::Unsupported)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum BufferId {
    Input = 0,
    Target = 1,
    TrainerParameters = 2,
    RefinerStimulus = 3,
    RefinerCapture = 4,
    RefinerExport = 5,
    TrainerComms = 6,
}

const BUFFER_IDS: [BufferId; 7] = [
    BufferId::Input,
    BufferId::Target,
    BufferId::TrainerParameters,
    BufferId::RefinerStimulus,
    BufferId::RefinerCapture,
    BufferId::RefinerExport,
    BufferId::TrainerComms,
];

/// Every reply must echo the complete identity, including the run generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RequestId {
    pub generation: u64,
    pub sequence: u64,
}

/// Meter streams must be re-bound at every state entry; old streams cannot advance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Epoch {
    pub generation: u64,
    pub entry: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BufferLease {
    pub id: BufferId,
    pub allocation: RequestId,
    pub address: u32,
    pub bytes: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sizes {
    pub input_bytes: u32,
    pub refine_bytes: u32,
}

impl Sizes {
    fn validate(self) -> Result<(), Refusal> {
        if [self.input_bytes, self.refine_bytes]
            .into_iter()
            .any(|n| n == 0 || n % 4 != 0 || n > MAX_STIMULUS_BYTES)
        {
            return Err(Refusal::InvalidSize);
        }
        Ok(())
    }

    pub fn buffer_bytes(self, id: BufferId) -> u32 {
        match id {
            BufferId::Input | BufferId::Target => self.input_bytes,
            BufferId::RefinerStimulus | BufferId::RefinerCapture => self.refine_bytes,
            BufferId::TrainerParameters => PARAMETER_BYTES,
            BufferId::TrainerComms => COMMS_BYTES,
            BufferId::RefinerExport => EXPORT_BYTES,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StockError {
    Allocation = 1,
    InputStimulus = 2,
    RefineStimulus = 3,
    MemoryWrite = 4,
    OutputFile = 5,
    MemoryRead = 6,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NcError {
    Stock(StockError),
    Sanity(u8),
    Backend,
    UnsupportedService,
    AllocationMismatch,
    NonFinite,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    Stale,
    DuplicateOrUnknown,
    NotSent,
    WrongReply,
    Busy,
    Unsupported,
    InvalidSize,
    NonFinite,
    Bound,
    IdentityExhausted,
}

/// These facts are intentionally deferred. They are not recovered routing laws.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Gate {
    EntryServices(NcState),
    PlayerStarted,
    TrainersStarted,
    Unmuted,
    DialogProgress,
    TrainerInputs,
    DiscardDecision,
    Quiesce,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum CallerFact {
    EntryServicesReady(NcState),
    PlayerStarted,
    TrainersStarted {
        conductor: bool,
        node: bool,
    },
    Unmuted,
    DialogProgress {
        minimum: f32,
    },
    /// Caller attests that parameters/comms are prepared by an actual backend.
    /// Seed generation and the trainer parameter law are not supplied here.
    TrainerInputsReady {
        seed: f32,
    },
    DiscardAllowed(bool),
    /// Caller attests all old DSP jobs are stopped, not that output is audible/silent.
    BackendQuiesced,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ParamWrite {
    pub node: u8,
    pub parameter: u8,
    pub value: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileRole {
    InputStimulus,
    RefineStimulus,
    RefinerCapture,
    InputCapture,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Operation {
    Allocate {
        id: BufferId,
        bytes: u32,
    },
    FreeBuffer(BufferLease),
    AwaitCaller(Gate),
    WriteFile {
        to: BufferLease,
        file: FileRole,
        bytes: u32,
    },
    ReadFile {
        from: BufferLease,
        file: FileRole,
        bytes: u32,
    },
    ReadModel {
        from: BufferLease,
        bytes: u32,
    },
    WriteModel {
        to: BufferLease,
        bytes: Vec<u8>,
    },
    /// An ordered batch. A failure may occur after any prefix was applied.
    SetParameters(Vec<ParamWrite>),
    RequestMeter {
        id: u8,
    },
    ScheduleExport,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Notice {
    State(NcState),
    Progress(f32),
    Error(NcError),
    CorruptCapture,
    /// Remote `toggle_ab_model` (VA 0x1a285c).
    AbModel(bool),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Effect {
    Request {
        id: RequestId,
        epoch: Epoch,
        operation: Operation,
    },
    /// This only asks the shell to withdraw queued work. It is not rollback.
    Withdraw(RequestId),
    Notify(Notice),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Reply {
    Allocated {
        id: BufferId,
        bytes: u32,
        address: u32,
    },
    Freed(BufferLease),
    Fact(CallerFact),
    /// Backend acknowledgement of execution, not a stock DSP ack or physical proof.
    Executed,
    TransferDone {
        lease: BufferLease,
        bytes: u32,
    },
    ModelRead {
        lease: BufferLease,
        bytes: Vec<u8>,
    },
    BulkDelay(f32),
    Sanity(u8),
    ExportDue,
    Failed(NcError),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    Open,
    Close,
    Next,
    Back,
    RemoteState(u8),
    RefineToggle,
    Export,
    RetryCleanup,
    /// RefineReady A/B (VA 0x3330e8): `true` = model audible (node1 p3 = 0), `false` = DUT.
    ToggleAb(bool),
    /// The shell inserted a new capture record (VA 0x3d3f18): clean up, then 1 → 6.
    Saved,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    Command(Command),
    /// The shell confirms dispatch, separately from execution/completion.
    Sent(RequestId),
    /// Authoritative confirmation that an offered request never reached the backend.
    NotSent(RequestId),
    Reply {
        request: RequestId,
        reply: Reply,
    },
    Progress {
        epoch: Epoch,
        value: f32,
    },
    Loss {
        epoch: Epoch,
        value: f32,
    },
    ExportCounter {
        epoch: Epoch,
        value: u32,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Delivery {
    Offered,
    Sent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum After {
    None,
    InitialParams,
    CalibrationWrite,
    CalibrationStart,
    CaptureRefinerRead,
    CaptureInputWrite,
    CaptureStart,
    CaptureTargetRead,
    TrainingStart,
    RefineStimulusWrite,
    RefineCaptureWrite,
    RefineStart,
    RefineStop,
    ManualStart,
    ManualStop,
    ExportStart,
    ExportRead,
    ExportRefresh,
    ExportRefreshCounter,
}

#[derive(Debug, Clone)]
struct Pending {
    id: RequestId,
    epoch: Epoch,
    operation: Operation,
    delivery: Delivery,
    abandoned: bool,
    after: After,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stage {
    Idle,
    Waiting,
    CapturingRefinerRead,
    CapturingFree,
    CapturingAllocate,
    Working,
    Completed,
    ExportWaiting,
}

/// Known allocations and outstanding requests survive cancellation until acknowledged.
///
/// `Clone` exists so `apply` can stage each event atomically. A shell must drive
/// exactly one instance; a clone duplicates obligations, not the backend.
#[derive(Debug, Clone)]
pub struct CaptureOrchestrator {
    sizes: Sizes,
    state: NcState,
    epoch: Epoch,
    sequence: u64,
    pending: Vec<Pending>,
    buffers: [Option<BufferLease>; 7],
    error: Option<NcError>,
    cleaning: bool,
    cleanup_paused: bool,
    quiesced: bool,
    services: bool,
    model_started: bool,
    unmuted: bool,
    /// State-1 services, player start and node1 p2 writes completed for this run.
    initial_setup: bool,
    progress_minimum: Option<f32>,
    seed: Option<f32>,
    progress: f32,
    loss: f32,
    delay: Option<f32>,
    sanity: Option<u8>,
    export_counter: u32,
    export_snapshot: u32,
    refresh_counter: f32,
    model: Option<Vec<u8>>,
    stage: Stage,
    /// Saved: cleanup completion enters 6 through the state-1 restoration path.
    resume_mic_setup: bool,
}

impl CaptureOrchestrator {
    pub fn new(sizes: Sizes) -> Result<Self, Refusal> {
        sizes.validate()?;
        Ok(Self {
            sizes,
            state: NcState::Inactive,
            epoch: Epoch {
                generation: 0,
                entry: 0,
            },
            sequence: 0,
            pending: Vec::new(),
            buffers: [None; 7],
            error: None,
            cleaning: false,
            cleanup_paused: false,
            quiesced: false,
            services: false,
            model_started: false,
            unmuted: false,
            initial_setup: false,
            progress_minimum: None,
            seed: None,
            progress: 0.0,
            loss: 100.0,
            delay: None,
            sanity: None,
            export_counter: 0,
            export_snapshot: 0,
            refresh_counter: 0.0,
            model: None,
            stage: Stage::Idle,
            resume_mic_setup: false,
        })
    }

    pub fn state(&self) -> NcState {
        self.state
    }
    pub fn epoch(&self) -> Epoch {
        self.epoch
    }
    pub fn error(&self) -> Option<NcError> {
        self.error
    }
    pub fn model(&self) -> Option<&[u8]> {
        self.model.as_deref()
    }
    pub fn buffer(&self, id: BufferId) -> Option<BufferLease> {
        self.buffers[id as usize]
    }

    /// False also while withdrawal, late-allocation or quiescence replies are owed.
    pub fn cleanup_complete(&self) -> bool {
        !self.cleaning && self.buffers.iter().all(Option::is_none) && self.pending.is_empty()
    }

    /// A cancellation abandoned `id`: a fact only drains the obligation, and cleanup waits
    /// for it.
    pub fn abandoned(&self, id: RequestId) -> bool {
        self.pending.iter().any(|p| p.id == id && p.abandoned)
    }

    pub fn pending_requests(&self) -> impl Iterator<Item = (RequestId, &Operation, bool)> {
        self.pending
            .iter()
            .map(|p| (p.id, &p.operation, p.delivery == Delivery::Sent))
    }

    /// Every refusal is atomic: the event is staged on a copy and committed only on success.
    pub fn apply(&mut self, event: Event) -> Result<Vec<Effect>, Refusal> {
        let mut next = self.clone();
        let effects = next.step(event)?;
        *self = next;
        Ok(effects)
    }

    /// Within `REQUEST_LIMIT` of `u64::MAX` on any counter: only drain and close, no new run.
    fn near_exhaustion(&self) -> bool {
        let headroom = u64::MAX - REQUEST_LIMIT as u64;
        [self.sequence, self.epoch.entry, self.epoch.generation]
            .into_iter()
            .any(|counter| counter > headroom)
    }

    fn step(&mut self, event: Event) -> Result<Vec<Effect>, Refusal> {
        // Stop new runs and progress before exhaustion, keeping headroom to drain and close.
        // Counters still use checked arithmetic; a staged refusal leaves no partial work.
        // Allowed draining events re-check at `restart` and the discard re-entry in `drive`.
        let near_exhaustion = self.near_exhaustion();
        let draining_or_cleanup = matches!(
            &event,
            Event::Sent(_)
                | Event::NotSent(_)
                | Event::Reply { .. }
                | Event::Command(Command::Close | Command::RetryCleanup)
        );
        if near_exhaustion && !draining_or_cleanup {
            return Err(Refusal::IdentityExhausted);
        }
        let mut effects = Vec::new();
        match event {
            Event::Command(command) => self.command(command, &mut effects)?,
            Event::Sent(id) => {
                let pending = self
                    .pending
                    .iter_mut()
                    .find(|p| p.id == id)
                    .ok_or(Refusal::DuplicateOrUnknown)?;
                if pending.delivery == Delivery::Sent {
                    return Err(Refusal::DuplicateOrUnknown);
                }
                pending.delivery = Delivery::Sent;
            }
            Event::NotSent(id) => {
                let index = self
                    .pending
                    .iter()
                    .position(|p| p.id == id)
                    .ok_or(Refusal::DuplicateOrUnknown)?;
                if self.pending[index].delivery != Delivery::Offered {
                    return Err(Refusal::WrongReply);
                }
                let pending = self.pending.remove(index);
                if !pending.abandoned {
                    self.fail_pausing(NcError::Backend, &pending.operation, &mut effects)?;
                }
            }
            Event::Reply { request, reply } => self.reply(request, reply, &mut effects)?,
            Event::Progress { epoch, value } => {
                self.check_epoch(epoch)?;
                if !value.is_finite() {
                    return Err(Refusal::NonFinite);
                }
                match self.state {
                    NcState::Calibrate | NcState::RefineCapture | NcState::Capturing => {
                        let minimum = self.progress_minimum.ok_or(Refusal::Busy)?;
                        if value < minimum {
                            return Ok(effects);
                        }
                        if self.stage != Stage::Working && self.state != NcState::RefineCapture {
                            return Err(Refusal::Busy);
                        }
                    }
                    NcState::Training | NcState::AutoRefine => {
                        if self.stage != Stage::Working {
                            return Err(Refusal::Busy);
                        }
                    }
                    NcState::RefineReady | NcState::ManualRefine | NcState::Exporting => {}
                    _ => return Err(Refusal::Unsupported),
                }
                self.progress = value;
                effects.push(Effect::Notify(Notice::Progress(value)));
            }
            Event::Loss { epoch, value } => {
                self.check_epoch(epoch)?;
                if !value.is_finite() {
                    return Err(Refusal::NonFinite);
                }
                if self.state != NcState::Training {
                    return Err(Refusal::Unsupported);
                }
                self.loss = value;
            }
            Event::ExportCounter { epoch, value } => {
                self.check_epoch(epoch)?;
                if !(12..=15).contains(&(self.state as u8)) {
                    return Err(Refusal::Unsupported);
                }
                self.export_counter = value;
            }
        }
        self.drive(&mut effects)?;
        Ok(effects)
    }

    fn check_epoch(&self, epoch: Epoch) -> Result<(), Refusal> {
        if epoch != self.epoch || self.cleaning {
            Err(Refusal::Stale)
        } else {
            Ok(())
        }
    }

    fn issue(
        &mut self,
        operation: Operation,
        after: After,
        effects: &mut Vec<Effect>,
    ) -> Result<(), Refusal> {
        if self.pending.len() == REQUEST_LIMIT {
            return Err(Refusal::Bound);
        }
        self.sequence = self
            .sequence
            .checked_add(1)
            .ok_or(Refusal::IdentityExhausted)?;
        let id = RequestId {
            generation: self.epoch.generation,
            sequence: self.sequence,
        };
        self.pending.push(Pending {
            id,
            epoch: self.epoch,
            operation: operation.clone(),
            delivery: Delivery::Offered,
            abandoned: false,
            after,
        });
        effects.push(Effect::Request {
            id,
            epoch: self.epoch,
            operation,
        });
        Ok(())
    }

    fn gate(&mut self, gate: Gate, effects: &mut Vec<Effect>) -> Result<(), Refusal> {
        self.issue(Operation::AwaitCaller(gate), After::None, effects)
    }

    fn parameters(
        &mut self,
        writes: &[(u8, u8, f32)],
        after: After,
        effects: &mut Vec<Effect>,
    ) -> Result<(), Refusal> {
        let writes = writes
            .iter()
            .map(|&(node, parameter, value)| ParamWrite {
                node,
                parameter,
                value,
            })
            .collect();
        self.issue(Operation::SetParameters(writes), after, effects)
    }

    fn allocate(&mut self, ids: &[BufferId], effects: &mut Vec<Effect>) -> Result<(), Refusal> {
        for &id in ids {
            if self.buffer(id).is_some() || self.pending.iter().any(|p| {
                matches!(p.operation, Operation::Allocate { id: pending, .. } if pending == id)
            }) { return Err(Refusal::Busy); }
        }
        for &id in ids {
            self.issue(
                Operation::Allocate {
                    id,
                    bytes: self.sizes.buffer_bytes(id),
                },
                After::None,
                effects,
            )?;
        }
        Ok(())
    }

    fn has_buffers(&self, ids: &[BufferId]) -> bool {
        ids.iter().all(|&id| self.buffer(id).is_some())
    }

    fn freeing(&self, lease: BufferLease) -> bool {
        self.pending
            .iter()
            .any(|p| matches!(p.operation, Operation::FreeBuffer(old) if old == lease))
    }

    fn free(&mut self, ids: &[BufferId], effects: &mut Vec<Effect>) -> Result<(), Refusal> {
        for &id in ids {
            if let Some(lease) = self.buffer(id)
                && !self.freeing(lease)
            {
                self.issue(Operation::FreeBuffer(lease), After::None, effects)?;
            }
        }
        Ok(())
    }

    fn command(&mut self, command: Command, effects: &mut Vec<Effect>) -> Result<(), Refusal> {
        if command == Command::Close {
            return self.cancel(NcState::Inactive, effects);
        }
        if command == Command::RetryCleanup {
            if !self.cleaning {
                return Err(Refusal::Unsupported);
            }
            self.cleanup_paused = false;
            return Ok(());
        }
        if self.cleaning {
            return Err(Refusal::Busy);
        }
        match command {
            Command::Open => self.restart(effects),
            Command::Next => {
                let target = match self.state {
                    NcState::Initial => NcState::DiagramStep1,
                    NcState::DiagramStep1 => NcState::DiagramStep2,
                    NcState::DiagramStep2 => NcState::DiagramStep3,
                    NcState::DiagramStep3 => NcState::DiagramStep4,
                    NcState::DiagramStep4 => NcState::MicSetup,
                    NcState::MicSetup => NcState::Calibrate,
                    NcState::HandleError => return self.restart(effects),
                    _ => return Err(Refusal::Unsupported), // RefineReady saves via `Command::Saved`.
                };
                if !self.pending.is_empty() || (target == NcState::Calibrate && !self.initial_setup)
                {
                    return Err(Refusal::Busy);
                }
                self.enter(target, effects)
            }
            Command::Back => {
                if (2..=5).contains(&(self.state as u8)) {
                    self.enter(NcState::try_from(self.state as u8 - 1)?, effects)
                } else if self.state == NcState::MicSetup {
                    self.cancel(NcState::Initial, effects)
                } else if self.state == NcState::Inactive {
                    Err(Refusal::Unsupported)
                } else {
                    if self.pending.iter().any(|p| {
                        matches!(p.operation, Operation::AwaitCaller(Gate::DiscardDecision))
                    }) {
                        return Err(Refusal::Busy);
                    }
                    self.gate(Gate::DiscardDecision, effects)
                }
            }
            Command::RemoteState(value) => {
                let target = NcState::try_from(value)?;
                if (1..=6).contains(&(self.state as u8)) && (1..=6).contains(&value) {
                    if target == self.state {
                        return Ok(());
                    }
                    if !self.pending.is_empty() {
                        return Err(Refusal::Busy);
                    }
                    self.enter(target, effects)
                } else if self.state == NcState::ManualRefine && target == NcState::RefineReady {
                    self.command(Command::RefineToggle, effects)
                } else if self.state == NcState::HandleError && target == NcState::Initial {
                    self.command(Command::Next, effects)
                } else {
                    Err(Refusal::Unsupported)
                }
            }
            Command::RefineToggle => {
                if !self.pending.is_empty() {
                    return Err(Refusal::Busy);
                }
                match self.state {
                    NcState::RefineReady => self.enter(NcState::ManualRefine, effects),
                    NcState::ManualRefine => self.enter(NcState::RefineReady, effects),
                    _ => Err(Refusal::Unsupported),
                }
            }
            Command::Export => {
                if self.state != NcState::RefineReady {
                    return Err(Refusal::Unsupported);
                }
                if !self.pending.is_empty() {
                    return Err(Refusal::Busy);
                }
                self.enter(NcState::Exporting, effects)
            }
            Command::ToggleAb(model) => {
                if self.state != NcState::RefineReady {
                    return Err(Refusal::Unsupported);
                }
                if !self.pending.is_empty() {
                    return Err(Refusal::Busy);
                }
                effects.push(Effect::Notify(Notice::AbModel(model)));
                let dut = if model { 0.0 } else { 1.0 };
                self.parameters(&[(1, 3, dut)], After::None, effects)
            }
            Command::Saved => {
                if self.state != NcState::RefineReady || self.model.is_none() {
                    return Err(Refusal::Unsupported);
                }
                if !self.pending.is_empty() {
                    return Err(Refusal::Busy);
                }
                self.cancel(NcState::Initial, effects)?;
                // Cleanup always waits for Quiesce, so it cannot complete inside `cancel`.
                self.resume_mic_setup = true;
                Ok(())
            }
            Command::Close | Command::RetryCleanup => unreachable!(),
        }
    }

    fn reset(&mut self) {
        self.error = None;
        self.progress = 0.0;
        self.loss = 100.0;
        self.delay = None;
        self.sanity = None;
        self.export_counter = 0;
        self.export_snapshot = 0;
        self.refresh_counter = 0.0;
        self.model = None;
        self.seed = None;
        self.initial_setup = false;
        self.stage = Stage::Idle;
    }

    /// Starts a new run generation at 1 (Open, Next/remote 1 from 16, discard from 16).
    /// From 16, `enter` keeps the stock remap to 6 and restores state-1 setup there.
    fn restart(&mut self, effects: &mut Vec<Effect>) -> Result<(), Refusal> {
        if !self.cleanup_complete() {
            return Err(Refusal::Busy);
        }
        if self.near_exhaustion() {
            return Err(Refusal::IdentityExhausted);
        }
        self.epoch.generation = self
            .epoch
            .generation
            .checked_add(1)
            .ok_or(Refusal::IdentityExhausted)?;
        self.enter(NcState::Initial, effects)
    }

    fn enter(&mut self, target: NcState, effects: &mut Vec<Effect>) -> Result<(), Refusal> {
        let from = self.state;
        self.state = target;
        self.epoch.entry = self
            .epoch
            .entry
            .checked_add(1)
            .ok_or(Refusal::IdentityExhausted)?;
        self.services = false;
        self.model_started = false;
        self.unmuted = false;
        self.progress_minimum = None;
        self.stage = Stage::Waiting;
        effects.push(Effect::Notify(Notice::State(target)));
        match target {
            NcState::Initial => {
                self.reset();
                // VA 0x338118: entering 1 from 16 rewrites to 6. Save (VA 0x3d3f18) sets 1 then 6.
                if from == NcState::HandleError || std::mem::take(&mut self.resume_mic_setup) {
                    return self.enter(NcState::MicSetup, effects);
                }
                self.gate(Gate::EntryServices(target), effects)?;
                self.gate(Gate::PlayerStarted, effects)?;
            }
            NcState::MicSetup if !self.initial_setup => {
                // VA 0x338118 remaps 16→1 to 6 and skips the state-1 signal connect; its
                // entry table lists no disconnect for 16. Stock DSPController routing/mute
                // across 16 is unresolved. Owned 16 has quiesced, so restore state-1 setup
                // here before 6 may advance to 7.
                self.stage = Stage::Idle;
                self.gate(Gate::EntryServices(NcState::Initial), effects)?;
                self.gate(Gate::PlayerStarted, effects)?;
            }
            NcState::Calibrate => {
                self.gate(Gate::EntryServices(target), effects)?;
                self.gate(Gate::Unmuted, effects)?;
                self.gate(Gate::DialogProgress, effects)?;
                self.allocate(
                    &[BufferId::RefinerCapture, BufferId::RefinerStimulus],
                    effects,
                )?;
            }
            NcState::RefineCapture => {
                self.gate(Gate::DialogProgress, effects)?;
                self.stage = Stage::Working;
            }
            NcState::Capturing => {
                self.gate(Gate::EntryServices(target), effects)?;
                self.gate(Gate::DialogProgress, effects)?;
                let lease = self.buffer(BufferId::RefinerCapture).ok_or(Refusal::Busy)?;
                self.stage = Stage::CapturingRefinerRead;
                self.issue(
                    Operation::ReadFile {
                        from: lease,
                        file: FileRole::RefinerCapture,
                        bytes: lease.bytes,
                    },
                    After::CaptureRefinerRead,
                    effects,
                )?;
            }
            NcState::SanityCheck => self.enter(NcState::Training, effects)?,
            NcState::Training => {
                self.gate(Gate::EntryServices(target), effects)?;
                self.gate(Gate::TrainersStarted, effects)?;
                self.gate(Gate::Unmuted, effects)?;
                self.gate(Gate::TrainerInputs, effects)?;
            }
            NcState::AutoRefine => {
                self.gate(Gate::EntryServices(target), effects)?;
                self.gate(Gate::PlayerStarted, effects)?;
                self.gate(Gate::Unmuted, effects)?;
                self.free(
                    &[BufferId::Input, BufferId::Target, BufferId::TrainerComms],
                    effects,
                )?;
                self.allocate(
                    &[
                        BufferId::RefinerStimulus,
                        BufferId::RefinerCapture,
                        BufferId::RefinerExport,
                    ],
                    effects,
                )?;
            }
            NcState::RefineReady => {
                if from == NcState::ManualRefine {
                    self.parameters(&[(1, 1, 0.0)], After::ManualStop, effects)?;
                } else {
                    self.stage = Stage::Completed;
                    if self.model.is_none() && from != NcState::Exporting {
                        self.issue(Operation::ScheduleExport, After::None, effects)?;
                    }
                }
            }
            NcState::ManualRefine => {
                self.parameters(&[(1, 1, 1.0)], After::ManualStart, effects)?
            }
            NcState::Exporting => {
                self.export_snapshot = self.export_counter;
                self.parameters(&[(1, 5, 1.0)], After::ExportStart, effects)?;
            }
            _ => self.stage = Stage::Idle,
        }
        Ok(())
    }

    fn cancel(&mut self, target: NcState, effects: &mut Vec<Effect>) -> Result<(), Refusal> {
        self.resume_mic_setup = false;
        if self.cleaning {
            if target == NcState::Inactive {
                self.state = target;
                effects.push(Effect::Notify(Notice::State(target)));
            }
            return Ok(());
        }
        self.epoch.generation = self
            .epoch
            .generation
            .checked_add(1)
            .ok_or(Refusal::IdentityExhausted)?;
        self.epoch.entry = self
            .epoch
            .entry
            .checked_add(1)
            .ok_or(Refusal::IdentityExhausted)?;
        self.state = target;
        let error = self.error;
        self.reset();
        self.error = error;
        self.services = false;
        self.model_started = false;
        self.unmuted = false;
        self.progress_minimum = None;
        // `cleanup_paused` is false while not cleaning, unless `fail_pausing` set it
        // for this cancellation; then the drive below must not offer Quiesce.
        self.cleaning = true;
        self.quiesced = false;
        effects.push(Effect::Notify(Notice::State(target)));
        for pending in &mut self.pending {
            if !matches!(pending.operation, Operation::FreeBuffer(_)) {
                pending.abandoned = true;
                if pending.delivery == Delivery::Offered {
                    effects.push(Effect::Withdraw(pending.id));
                }
            }
        }
        self.drive(effects)
    }

    fn fail(&mut self, error: NcError, effects: &mut Vec<Effect>) -> Result<(), Refusal> {
        self.error = Some(error);
        effects.push(Effect::Notify(Notice::Error(error)));
        if self.cleaning {
            return Ok(());
        }
        self.cancel(NcState::HandleError, effects)
    }

    /// A failed or never-sent free/Quiesce pauses cleanup before the cancellation
    /// drive, so no new cleanup work is offered until `RetryCleanup`.
    fn fail_pausing(
        &mut self,
        error: NcError,
        operation: &Operation,
        effects: &mut Vec<Effect>,
    ) -> Result<(), Refusal> {
        if matches!(
            operation,
            Operation::FreeBuffer(_) | Operation::AwaitCaller(Gate::Quiesce)
        ) {
            self.cleanup_paused = true;
        }
        self.fail(error, effects)
    }

    fn reply(
        &mut self,
        request: RequestId,
        reply: Reply,
        effects: &mut Vec<Effect>,
    ) -> Result<(), Refusal> {
        let index = self
            .pending
            .iter()
            .position(|p| p.id == request)
            .ok_or(Refusal::DuplicateOrUnknown)?;
        let pending = &self.pending[index];
        if pending.delivery != Delivery::Sent {
            return Err(Refusal::NotSent);
        }
        Self::validate_reply(&pending.operation, &reply)?;
        let pending = self.pending.remove(index);
        if let Reply::Failed(error) = reply {
            return self.fail_pausing(error, &pending.operation, effects);
        }
        match (&pending.operation, reply) {
            (
                Operation::Allocate { id, bytes },
                Reply::Allocated {
                    id: _,
                    bytes: actual,
                    address,
                },
            ) => {
                // Owned policy is stricter than stock: exact size and nonzero address for all ids.
                if actual != *bytes || address == 0 || address.checked_add(actual).is_none() {
                    // Even an invalid nonzero allocation needs a cleanup obligation.
                    if address != 0 {
                        self.buffers[*id as usize] = Some(BufferLease {
                            id: *id,
                            allocation: request,
                            address,
                            bytes: actual,
                        });
                    }
                    self.fail(NcError::AllocationMismatch, effects)?;
                } else {
                    self.buffers[*id as usize] = Some(BufferLease {
                        id: *id,
                        allocation: request,
                        address,
                        bytes: actual,
                    });
                }
            }
            (Operation::FreeBuffer(lease), Reply::Freed(_)) => {
                if self.buffer(lease.id) != Some(*lease) {
                    return Err(Refusal::Stale);
                }
                self.buffers[lease.id as usize] = None;
            }
            (Operation::AwaitCaller(Gate::Quiesce), Reply::Fact(CallerFact::BackendQuiesced)) => {
                self.quiesced = true;
            }
            (_, _) if pending.abandoned || pending.epoch != self.epoch => {
                // Drain the obligation, but never run its continuation in another entry/run.
            }
            (Operation::AwaitCaller(_), Reply::Fact(fact)) => self.fact(fact, effects)?,
            (Operation::RequestMeter { id: 5 }, Reply::BulkDelay(value)) => {
                self.delay = Some(value)
            }
            (Operation::RequestMeter { id: 6 }, Reply::Sanity(bits)) => self.sanity = Some(bits),
            (Operation::ReadModel { .. }, Reply::ModelRead { bytes, .. }) => {
                if !valid_export_header(&bytes) {
                    self.model = None;
                    effects.push(Effect::Notify(Notice::CorruptCapture));
                    self.enter(NcState::RefineReady, effects)?;
                } else {
                    self.model = Some(bytes);
                    self.finish(pending.after, effects)?;
                }
            }
            (Operation::ScheduleExport, Reply::ExportDue) => {
                self.enter(NcState::Exporting, effects)?
            }
            (_, _) => self.finish(pending.after, effects)?,
        }
        Ok(())
    }

    fn validate_reply(operation: &Operation, reply: &Reply) -> Result<(), Refusal> {
        if matches!(reply, Reply::Failed(_)) {
            return Ok(());
        }
        let valid = match (operation, reply) {
            (Operation::Allocate { id, .. }, Reply::Allocated { id: actual, .. }) => id == actual,
            (Operation::FreeBuffer(lease), Reply::Freed(actual)) => lease == actual,
            (Operation::AwaitCaller(gate), Reply::Fact(fact)) => match (gate, fact) {
                (Gate::EntryServices(state), CallerFact::EntryServicesReady(actual)) => {
                    state == actual
                }
                (Gate::PlayerStarted, CallerFact::PlayerStarted)
                | (Gate::Unmuted, CallerFact::Unmuted)
                | (Gate::DiscardDecision, CallerFact::DiscardAllowed(_))
                | (Gate::Quiesce, CallerFact::BackendQuiesced) => true,
                (Gate::TrainersStarted, CallerFact::TrainersStarted { conductor, node }) => {
                    *conductor && *node
                }
                (Gate::DialogProgress, CallerFact::DialogProgress { minimum }) => {
                    if !minimum.is_finite() {
                        return Err(Refusal::NonFinite);
                    }
                    true
                }
                (Gate::TrainerInputs, CallerFact::TrainerInputsReady { seed }) => {
                    if !seed.is_finite() {
                        return Err(Refusal::NonFinite);
                    }
                    true
                }
                _ => false,
            },
            (Operation::SetParameters(_), Reply::Executed) => true,
            (
                Operation::WriteFile { to, bytes, .. },
                Reply::TransferDone {
                    lease,
                    bytes: actual,
                },
            )
            | (
                Operation::ReadFile {
                    from: to, bytes, ..
                },
                Reply::TransferDone {
                    lease,
                    bytes: actual,
                },
            ) => to == lease && bytes == actual,
            (
                Operation::WriteModel { to, bytes },
                Reply::TransferDone {
                    lease,
                    bytes: actual,
                },
            ) => to == lease && bytes.len() == *actual as usize,
            (
                Operation::ReadModel { from, bytes },
                Reply::ModelRead {
                    lease,
                    bytes: actual,
                },
            ) => from == lease && actual.len() == *bytes as usize,
            (Operation::RequestMeter { id: 5 }, Reply::BulkDelay(value)) => {
                if !value.is_finite() {
                    return Err(Refusal::NonFinite);
                }
                true
            }
            (Operation::RequestMeter { id: 6 }, Reply::Sanity(_))
            | (Operation::ScheduleExport, Reply::ExportDue) => true,
            _ => false,
        };
        if valid {
            Ok(())
        } else {
            Err(Refusal::WrongReply)
        }
    }

    fn fact(&mut self, fact: CallerFact, effects: &mut Vec<Effect>) -> Result<(), Refusal> {
        match fact {
            CallerFact::EntryServicesReady(_) => self.services = true,
            CallerFact::PlayerStarted | CallerFact::TrainersStarted { .. } => {
                self.model_started = true
            }
            CallerFact::Unmuted => self.unmuted = true,
            CallerFact::DialogProgress { minimum } => self.progress_minimum = Some(minimum),
            CallerFact::TrainerInputsReady { seed } => self.seed = Some(seed),
            // 16 has already drained and quiesced; restart keeps the stock 1 -> 6 remap.
            CallerFact::DiscardAllowed(true) if self.state == NcState::HandleError => {
                self.restart(effects)?
            }
            CallerFact::DiscardAllowed(true) => self.cancel(NcState::Initial, effects)?,
            CallerFact::DiscardAllowed(false) | CallerFact::BackendQuiesced => {}
        }
        Ok(())
    }

    fn write_file(
        &mut self,
        id: BufferId,
        file: FileRole,
        after: After,
        effects: &mut Vec<Effect>,
    ) -> Result<(), Refusal> {
        let lease = self.buffer(id).ok_or(Refusal::Busy)?;
        self.issue(
            Operation::WriteFile {
                to: lease,
                file,
                bytes: lease.bytes,
            },
            after,
            effects,
        )
    }

    fn finish(&mut self, after: After, effects: &mut Vec<Effect>) -> Result<(), Refusal> {
        match after {
            After::None => {}
            After::InitialParams => {
                self.stage = Stage::Completed;
                self.initial_setup = true;
            }
            After::CalibrationWrite => self.parameters(
                &[(1, 0, (self.sizes.refine_bytes / 4) as f32), (1, 1, 1.0)],
                After::CalibrationStart,
                effects,
            )?,
            After::CalibrationStart => {
                self.stage = Stage::Working;
                self.progress = 0.0;
            }
            After::CaptureRefinerRead => {
                self.stage = Stage::CapturingFree;
                self.free(
                    &[BufferId::RefinerStimulus, BufferId::RefinerCapture],
                    effects,
                )?;
            }
            After::CaptureInputWrite => self.parameters(
                &[(1, 0, (self.sizes.input_bytes / 4) as f32), (1, 3, 1.0)],
                After::CaptureStart,
                effects,
            )?,
            After::CaptureStart
            | After::TrainingStart
            | After::RefineStart
            | After::ManualStart => self.stage = Stage::Working,
            After::CaptureTargetRead => self.enter(NcState::SanityCheck, effects)?,
            After::RefineStimulusWrite => self.write_file(
                BufferId::RefinerCapture,
                FileRole::RefinerCapture,
                After::RefineCaptureWrite,
                effects,
            )?,
            After::RefineCaptureWrite => {
                let delay = self.delay.ok_or(Refusal::Busy)?;
                self.parameters(
                    &[(1, 4, 1.0), (1, 2, delay), (1, 0, 1.0)],
                    After::RefineStart,
                    effects,
                )?;
            }
            After::RefineStop => self.enter(NcState::RefineReady, effects)?,
            After::ManualStop => {
                self.stage = Stage::Completed;
                if self.model.is_none() {
                    self.issue(Operation::ScheduleExport, After::None, effects)?;
                }
            }
            After::ExportStart => self.stage = Stage::ExportWaiting,
            After::ExportRead => {
                let lease = self.buffer(BufferId::RefinerExport).ok_or(Refusal::Busy)?;
                let bytes = self.model.as_ref().ok_or(Refusal::Busy)?.clone();
                self.issue(
                    Operation::WriteModel { to: lease, bytes },
                    After::ExportRefresh,
                    effects,
                )?;
            }
            After::ExportRefresh => {
                self.refresh_counter += 1.0;
                self.parameters(
                    &[(1, 6, self.refresh_counter)],
                    After::ExportRefreshCounter,
                    effects,
                )?;
            }
            After::ExportRefreshCounter => self.enter(NcState::RefineReady, effects)?,
        }
        Ok(())
    }

    fn drive(&mut self, effects: &mut Vec<Effect>) -> Result<(), Refusal> {
        if self.cleaning {
            // Drain even queued requests first. Otherwise an old start write could
            // execute after quiescence and make an earlier stop acknowledgement stale.
            if self.cleanup_paused || self.pending.iter().any(|p| p.abandoned) {
                return Ok(());
            }
            if !self.quiesced
                && !self
                    .pending
                    .iter()
                    .any(|p| matches!(p.operation, Operation::AwaitCaller(Gate::Quiesce)))
            {
                self.gate(Gate::Quiesce, effects)?;
            }
            if self.quiesced {
                self.free(&BUFFER_IDS, effects)?;
                if self.pending.is_empty() && self.buffers.iter().all(Option::is_none) {
                    self.cleaning = false;
                    // Discard re-entry starts the next run; `enter` clears the error.
                    // Inside headroom the completing reply is refused; Close retargets to 0.
                    if self.state == NcState::Initial {
                        if self.near_exhaustion() {
                            return Err(Refusal::IdentityExhausted);
                        }
                        self.enter(NcState::Initial, effects)?;
                    }
                }
            }
            return Ok(());
        }
        match self.state {
            NcState::Initial | NcState::MicSetup
                if self.stage == Stage::Idle && self.services && self.model_started =>
            {
                self.stage = Stage::Waiting;
                self.parameters(&[(1, 2, 1.0), (1, 2, 1.0)], After::InitialParams, effects)?;
            }
            NcState::Calibrate => {
                if self.stage == Stage::Waiting
                    && self.services
                    && self.unmuted
                    && self.progress_minimum.is_some()
                    && self.has_buffers(&[BufferId::RefinerStimulus, BufferId::RefinerCapture])
                {
                    self.stage = Stage::Completed;
                    self.write_file(
                        BufferId::RefinerStimulus,
                        FileRole::RefineStimulus,
                        After::CalibrationWrite,
                        effects,
                    )?;
                } else if self.stage == Stage::Working && self.progress >= 5.0 {
                    self.enter(NcState::RefineCapture, effects)?;
                }
            }
            NcState::RefineCapture => {
                if self.progress_minimum.is_some()
                    && (5.0..=9.9).contains(&self.progress)
                    && self.delay.is_none()
                    && !self
                        .pending
                        .iter()
                        .any(|p| matches!(p.operation, Operation::RequestMeter { id: 5 }))
                {
                    self.issue(Operation::RequestMeter { id: 5 }, After::None, effects)?;
                }
                if self.delay.is_some()
                    && (self.progress - 9.9).abs() <= 9.9 * 1e-5
                    && self.progress_minimum.is_some()
                {
                    self.enter(NcState::Capturing, effects)?;
                }
            }
            NcState::Capturing => {
                if self.stage == Stage::CapturingFree
                    && self.buffer(BufferId::RefinerStimulus).is_none()
                    && self.buffer(BufferId::RefinerCapture).is_none()
                {
                    self.stage = Stage::CapturingAllocate;
                    self.allocate(
                        &[
                            BufferId::Input,
                            BufferId::Target,
                            BufferId::TrainerParameters,
                            BufferId::TrainerComms,
                        ],
                        effects,
                    )?;
                }
                if self.stage == Stage::CapturingAllocate
                    && self.services
                    && self.progress_minimum.is_some()
                    && self.has_buffers(&[
                        BufferId::Input,
                        BufferId::Target,
                        BufferId::TrainerParameters,
                        BufferId::TrainerComms,
                    ])
                {
                    self.stage = Stage::Completed;
                    self.write_file(
                        BufferId::Input,
                        FileRole::InputStimulus,
                        After::CaptureInputWrite,
                        effects,
                    )?;
                }
                if self.stage == Stage::Working && self.progress == 30.0 {
                    match self.sanity {
                        Some(0) => {
                            self.stage = Stage::Completed;
                            let lease = self.buffer(BufferId::Target).ok_or(Refusal::Busy)?;
                            self.issue(
                                Operation::ReadFile {
                                    from: lease,
                                    file: FileRole::InputCapture,
                                    bytes: lease.bytes,
                                },
                                After::CaptureTargetRead,
                                effects,
                            )?;
                        }
                        Some(bits) => self.fail(NcError::Sanity(bits), effects)?,
                        None if !self
                            .pending
                            .iter()
                            .any(|p| matches!(p.operation, Operation::RequestMeter { id: 6 })) =>
                        {
                            self.issue(Operation::RequestMeter { id: 6 }, After::None, effects)?
                        }
                        None => {}
                    }
                }
            }
            NcState::Training => {
                if self.stage == Stage::Waiting
                    && self.services
                    && self.model_started
                    && self.unmuted
                    && self.seed.is_some()
                    && self.has_buffers(&[BufferId::TrainerParameters, BufferId::TrainerComms])
                {
                    self.stage = Stage::Completed;
                    let seed = self.seed.expect("checked seed");
                    self.parameters(
                        &[(0, 4, seed), (4, 4, seed), (0, 0, 1.0), (4, 0, 1.0)],
                        After::TrainingStart,
                        effects,
                    )?;
                } else if self.stage == Stage::Working && self.progress == 90.0 {
                    self.enter(NcState::AutoRefine, effects)?;
                }
            }
            NcState::AutoRefine => {
                if self.stage == Stage::Waiting
                    && self.services
                    && self.model_started
                    && self.unmuted
                    && self.has_buffers(&[
                        BufferId::RefinerStimulus,
                        BufferId::RefinerCapture,
                        BufferId::RefinerExport,
                    ])
                    && [BufferId::Input, BufferId::Target, BufferId::TrainerComms]
                        .into_iter()
                        .all(|id| self.buffer(id).is_none())
                {
                    self.stage = Stage::Completed;
                    self.write_file(
                        BufferId::RefinerStimulus,
                        FileRole::RefineStimulus,
                        After::RefineStimulusWrite,
                        effects,
                    )?;
                } else if self.stage == Stage::Working && self.progress == 100.0 {
                    self.stage = Stage::Completed;
                    self.parameters(&[(1, 0, 0.0), (1, 3, 0.0)], After::RefineStop, effects)?;
                }
            }
            NcState::Exporting
                if self.stage == Stage::ExportWaiting
                    && self.export_counter != self.export_snapshot =>
            {
                self.stage = Stage::Completed;
                let lease = self.buffer(BufferId::RefinerExport).ok_or(Refusal::Busy)?;
                self.issue(
                    Operation::ReadModel {
                        from: lease,
                        bytes: EXPORT_BYTES,
                    },
                    After::ExportRead,
                    effects,
                )?;
            }
            _ => {}
        }
        Ok(())
    }
}

/// Only the stock export header gate (VA 0x335704), not full model validation.
pub fn valid_export_header(bytes: &[u8]) -> bool {
    if !(12..=0x2fff).contains(&bytes.len()) {
        return false;
    }
    let sample_rate = f32::from_le_bytes(bytes[0..4].try_into().expect("header word"));
    let pre = u32::from_le_bytes(bytes[4..8].try_into().expect("header word"));
    let post = u32::from_le_bytes(bytes[8..12].try_into().expect("header word"));
    sample_rate.is_finite()
        && (sample_rate - 48_000.0).abs() <= 48_000.0 * 1e-5
        && pre == 0x200
        && post == 0x800
}

#[cfg(test)]
mod tests {
    use super::*;

    fn machine() -> CaptureOrchestrator {
        CaptureOrchestrator::new(Sizes {
            input_bytes: 64,
            refine_bytes: 96,
        })
        .unwrap()
    }

    fn find(
        machine: &CaptureOrchestrator,
        predicate: impl Fn(&Operation) -> bool,
    ) -> (RequestId, Operation) {
        machine
            .pending_requests()
            .find(|(_, op, _)| predicate(op))
            .map(|(id, op, _)| (id, op.clone()))
            .expect("expected request")
    }

    fn respond(machine: &mut CaptureOrchestrator, id: RequestId, reply: Reply) -> Vec<Effect> {
        let sent = machine
            .pending_requests()
            .find(|(pending, _, _)| *pending == id)
            .unwrap()
            .2;
        if !sent {
            machine.apply(Event::Sent(id)).unwrap();
        }
        machine.apply(Event::Reply { request: id, reply }).unwrap()
    }

    // Synthetic service acknowledgements exercise ordering only, never train a model.
    fn acknowledge(
        machine: &mut CaptureOrchestrator,
        id: RequestId,
        operation: Operation,
    ) -> Vec<Effect> {
        let reply = match operation {
            Operation::Allocate { id, bytes } => Reply::Allocated {
                id,
                bytes,
                address: 0x1000 + (id as u32) * 0x10000,
            },
            Operation::FreeBuffer(lease) => Reply::Freed(lease),
            Operation::AwaitCaller(gate) => Reply::Fact(match gate {
                Gate::EntryServices(state) => CallerFact::EntryServicesReady(state),
                Gate::PlayerStarted => CallerFact::PlayerStarted,
                Gate::TrainersStarted => CallerFact::TrainersStarted {
                    conductor: true,
                    node: true,
                },
                Gate::Unmuted => CallerFact::Unmuted,
                Gate::DialogProgress => CallerFact::DialogProgress { minimum: 0.0 },
                Gate::TrainerInputs => CallerFact::TrainerInputsReady { seed: 0.25 },
                Gate::Quiesce => CallerFact::BackendQuiesced,
                Gate::DiscardDecision => panic!("discard decision must be explicit in the test"),
            }),
            Operation::SetParameters(_) => Reply::Executed,
            Operation::WriteFile {
                to: lease, bytes, ..
            }
            | Operation::ReadFile {
                from: lease, bytes, ..
            } => Reply::TransferDone { lease, bytes },
            Operation::WriteModel { to: lease, bytes } => Reply::TransferDone {
                lease,
                bytes: bytes.len() as u32,
            },
            _ => panic!("meter/timer/model data must be explicit in the test"),
        };
        respond(machine, id, reply)
    }

    fn settle(machine: &mut CaptureOrchestrator) -> Vec<Effect> {
        let mut effects = Vec::new();
        for _ in 0..48 {
            let next = machine
                .pending_requests()
                .find(|(_, operation, _)| {
                    !matches!(
                        operation,
                        Operation::RequestMeter { .. }
                            | Operation::ScheduleExport
                            | Operation::ReadModel { .. }
                            | Operation::AwaitCaller(Gate::DiscardDecision)
                    )
                })
                .map(|(id, op, _)| (id, op.clone()));
            let Some((id, op)) = next else {
                return effects;
            };
            effects.extend(acknowledge(machine, id, op));
        }
        panic!("synthetic acknowledgement loop exceeded its bound");
    }

    fn progress(machine: &mut CaptureOrchestrator, value: f32) -> Vec<Effect> {
        machine
            .apply(Event::Progress {
                epoch: machine.epoch(),
                value,
            })
            .unwrap()
    }

    fn calibrating() -> CaptureOrchestrator {
        let mut machine = machine();
        machine.apply(Event::Command(Command::Open)).unwrap();
        settle(&mut machine);
        for _ in 0..6 {
            machine.apply(Event::Command(Command::Next)).unwrap();
        }
        assert_eq!(machine.state(), NcState::Calibrate);
        settle(&mut machine);
        assert_eq!(machine.stage, Stage::Working);
        machine
    }

    fn capturing() -> CaptureOrchestrator {
        let mut machine = calibrating();
        progress(&mut machine, 5.0);
        settle(&mut machine);
        let (request, _) = find(&machine, |op| {
            matches!(op, Operation::RequestMeter { id: 5 })
        });
        respond(&mut machine, request, Reply::BulkDelay(12.5));
        assert_eq!(machine.state(), NcState::RefineCapture);
        progress(&mut machine, 9.9);
        settle(&mut machine);
        assert_eq!(machine.state(), NcState::Capturing);
        assert_eq!(machine.stage, Stage::Working);
        machine
    }

    fn training() -> CaptureOrchestrator {
        let mut machine = capturing();
        progress(&mut machine, 30.0);
        let (request, _) = find(&machine, |op| {
            matches!(op, Operation::RequestMeter { id: 6 })
        });
        respond(&mut machine, request, Reply::Sanity(0));
        let effects = settle(&mut machine);
        assert!(effects.contains(&Effect::Notify(Notice::State(NcState::SanityCheck))));
        assert_eq!(machine.state(), NcState::Training);
        assert_eq!(machine.stage, Stage::Working);
        machine
    }

    fn exporting() -> CaptureOrchestrator {
        let mut machine = training();
        progress(&mut machine, 90.0);
        settle(&mut machine);
        assert_eq!(machine.state(), NcState::AutoRefine);
        progress(&mut machine, 100.0);
        settle(&mut machine);
        assert_eq!(machine.state(), NcState::RefineReady);
        let (request, _) = find(&machine, |op| matches!(op, Operation::ScheduleExport));
        respond(&mut machine, request, Reply::ExportDue);
        settle(&mut machine);
        assert_eq!(machine.state(), NcState::Exporting);
        machine
    }

    fn synthetic_export() -> Vec<u8> {
        let mut bytes = vec![0; EXPORT_BYTES as usize];
        bytes[..4].copy_from_slice(&48_000.0_f32.to_le_bytes());
        bytes[4..8].copy_from_slice(&0x200_u32.to_le_bytes());
        bytes[8..12].copy_from_slice(&0x800_u32.to_le_bytes());
        bytes
    }

    #[test]
    fn connected_flow_keeps_exact_thresholds_and_export_completion_gates() {
        let mut machine = capturing();
        progress(&mut machine, 30.000_002);
        assert!(
            !machine
                .pending_requests()
                .any(|(_, op, _)| matches!(op, Operation::RequestMeter { id: 6 }))
        );
        progress(&mut machine, 30.0);
        let (request, _) = find(&machine, |op| {
            matches!(op, Operation::RequestMeter { id: 6 })
        });
        progress(&mut machine, 30.0);
        assert_eq!(
            machine
                .pending_requests()
                .filter(|(_, op, _)| matches!(op, Operation::RequestMeter { id: 6 }))
                .count(),
            1
        );
        respond(&mut machine, request, Reply::Sanity(0));
        settle(&mut machine);
        assert_eq!(machine.state(), NcState::Training);
        progress(&mut machine, 90.000_01);
        assert_eq!(machine.state(), NcState::Training);
        progress(&mut machine, 90.0);
        settle(&mut machine);
        assert_eq!(machine.state(), NcState::AutoRefine);
        assert_eq!(machine.stage, Stage::Working);
        assert_eq!(machine.delay, Some(12.5));
        progress(&mut machine, 100.000_01);
        assert_eq!(machine.state(), NcState::AutoRefine);
        progress(&mut machine, 100.0);
        let (stop, operation) = find(&machine, |op| matches!(op, Operation::SetParameters(_)));
        assert_eq!(
            operation,
            Operation::SetParameters(vec![
                ParamWrite {
                    node: 1,
                    parameter: 0,
                    value: 0.0
                },
                ParamWrite {
                    node: 1,
                    parameter: 3,
                    value: 0.0
                },
            ])
        );
        assert_eq!(machine.state(), NcState::AutoRefine);
        acknowledge(&mut machine, stop, operation);
        assert_eq!(machine.state(), NcState::RefineReady);
        assert!(machine.buffer(BufferId::Input).is_none());
        assert!(machine.buffer(BufferId::TrainerParameters).is_some());
    }

    #[test]
    fn export_requires_counter_exact_buffer_header_refresh_and_execution_ack() {
        let mut machine = exporting();
        machine
            .apply(Event::ExportCounter {
                epoch: machine.epoch(),
                value: 0,
            })
            .unwrap();
        assert_eq!(machine.pending_requests().count(), 0);
        machine
            .apply(Event::ExportCounter {
                epoch: machine.epoch(),
                value: 1,
            })
            .unwrap();
        let (request, op) = find(&machine, |op| matches!(op, Operation::ReadModel { .. }));
        let Operation::ReadModel { from: lease, bytes } = op else {
            unreachable!()
        };
        assert_eq!(bytes, EXPORT_BYTES);
        machine.apply(Event::Sent(request)).unwrap();
        assert_eq!(
            machine.apply(Event::Reply {
                request,
                reply: Reply::ModelRead {
                    lease,
                    bytes: vec![0; 12]
                }
            }),
            Err(Refusal::WrongReply)
        );
        respond(
            &mut machine,
            request,
            Reply::ModelRead {
                lease,
                bytes: synthetic_export(),
            },
        );
        assert_eq!(machine.state(), NcState::Exporting);
        let (write, operation) = find(&machine, |op| matches!(op, Operation::WriteModel { .. }));
        acknowledge(&mut machine, write, operation);
        let (refresh, operation) = find(&machine, |op| matches!(op, Operation::SetParameters(_)));
        assert_eq!(
            operation,
            Operation::SetParameters(vec![ParamWrite {
                node: 1,
                parameter: 6,
                value: 1.0
            }])
        );
        machine.apply(Event::Sent(refresh)).unwrap();
        assert_eq!(machine.state(), NcState::Exporting);
        respond(&mut machine, refresh, Reply::Executed);
        assert_eq!(machine.state(), NcState::RefineReady);
        assert_eq!(machine.model().unwrap().len(), EXPORT_BYTES as usize);
        assert_eq!(
            machine.apply(Event::Reply {
                request: refresh,
                reply: Reply::Executed
            }),
            Err(Refusal::DuplicateOrUnknown)
        );
        assert_eq!(
            machine.apply(Event::Command(Command::Next)),
            Err(Refusal::Unsupported)
        );
    }

    #[test]
    fn corrupt_export_stays_ready_without_fake_model_or_automatic_retry() {
        let mut machine = exporting();
        machine
            .apply(Event::ExportCounter {
                epoch: machine.epoch(),
                value: 7,
            })
            .unwrap();
        let (request, operation) = find(&machine, |op| matches!(op, Operation::ReadModel { .. }));
        let Operation::ReadModel { from: lease, .. } = operation else {
            unreachable!()
        };
        let effects = respond(
            &mut machine,
            request,
            Reply::ModelRead {
                lease,
                bytes: vec![0; EXPORT_BYTES as usize],
            },
        );
        assert!(effects.contains(&Effect::Notify(Notice::CorruptCapture)));
        assert_eq!(machine.state(), NcState::RefineReady);
        assert!(machine.model().is_none());
        assert_eq!(machine.pending_requests().count(), 0);
        assert!(!valid_export_header(&[0; 11]));
        let mut bytes = synthetic_export();
        bytes[..4].copy_from_slice(&f32::NAN.to_le_bytes());
        assert!(!valid_export_header(&bytes));
    }

    #[test]
    fn cancellation_retains_late_allocations_and_withdrawal_free_obligations() {
        let mut machine = machine();
        machine.apply(Event::Command(Command::Open)).unwrap();
        settle(&mut machine);
        machine
            .apply(Event::Command(Command::RemoteState(6)))
            .unwrap();
        machine.apply(Event::Command(Command::Next)).unwrap();
        let old_epoch = machine.epoch();
        let (late, operation) = find(&machine, |op| {
            matches!(
                op,
                Operation::Allocate {
                    id: BufferId::RefinerCapture,
                    ..
                }
            )
        });
        machine.apply(Event::Sent(late)).unwrap();
        let effects = machine.apply(Event::Command(Command::Close)).unwrap();
        assert!(effects.iter().any(|e| matches!(e, Effect::Withdraw(_))));
        assert_eq!(
            machine.apply(Event::Progress {
                epoch: old_epoch,
                value: 5.0
            }),
            Err(Refusal::Stale)
        );
        assert!(
            !machine
                .pending_requests()
                .any(|(_, op, _)| matches!(op, Operation::AwaitCaller(Gate::Quiesce)))
        );
        assert!(!machine.cleanup_complete());
        let withdrawals: Vec<_> = machine
            .pending
            .iter()
            .filter(|p| p.abandoned && p.delivery == Delivery::Offered)
            .map(|p| p.id)
            .collect();
        for id in withdrawals {
            machine.apply(Event::NotSent(id)).unwrap();
        }
        acknowledge(&mut machine, late, operation);
        let (quiesce, _) = find(&machine, |op| {
            matches!(op, Operation::AwaitCaller(Gate::Quiesce))
        });
        let effects = respond(
            &mut machine,
            quiesce,
            Reply::Fact(CallerFact::BackendQuiesced),
        );
        assert!(effects.iter().any(|e| matches!(
            e,
            Effect::Request {
                operation: Operation::FreeBuffer(_),
                ..
            }
        )));
        let lease = machine.buffer(BufferId::RefinerCapture).unwrap();
        assert!(!machine.cleanup_complete());
        let (free, _) = find(&machine, |op| matches!(op, Operation::FreeBuffer(_)));
        machine.apply(Event::Sent(free)).unwrap();
        assert_eq!(machine.buffer(BufferId::RefinerCapture), Some(lease));
        assert_eq!(
            machine.apply(Event::Command(Command::Open)),
            Err(Refusal::Busy)
        );
        respond(&mut machine, free, Reply::Failed(NcError::Backend));
        assert_eq!(machine.buffer(BufferId::RefinerCapture), Some(lease));
        assert!(!machine.cleanup_complete());
        assert_eq!(machine.pending_requests().count(), 0);
        machine
            .apply(Event::Command(Command::RetryCleanup))
            .unwrap();
        let (retry, _) = find(&machine, |op| matches!(op, Operation::FreeBuffer(_)));
        assert_ne!(retry, free);
        assert_eq!(
            machine.apply(Event::Reply {
                request: free,
                reply: Reply::Freed(lease)
            }),
            Err(Refusal::DuplicateOrUnknown)
        );
        respond(&mut machine, retry, Reply::Freed(lease));
        assert!(machine.cleanup_complete());
        assert_eq!(
            machine.apply(Event::Reply {
                request: late,
                reply: Reply::Allocated {
                    id: lease.id,
                    bytes: lease.bytes,
                    address: lease.address
                }
            }),
            Err(Refusal::DuplicateOrUnknown)
        );
        machine.apply(Event::Command(Command::Open)).unwrap();
        assert_ne!(machine.epoch().generation, old_epoch.generation);
    }

    #[test]
    fn late_transfer_failure_does_not_resume_canceled_run_or_free_early() {
        let mut machine = calibrating();
        progress(&mut machine, 5.0);
        settle(&mut machine);
        let (delay, _) = find(&machine, |op| {
            matches!(op, Operation::RequestMeter { id: 5 })
        });
        respond(&mut machine, delay, Reply::BulkDelay(8.0));
        progress(&mut machine, 9.9);
        let (read, _) = find(&machine, |op| matches!(op, Operation::ReadFile { .. }));
        machine.apply(Event::Sent(read)).unwrap();
        machine.apply(Event::Command(Command::Close)).unwrap();
        let withdrawals: Vec<_> = machine
            .pending
            .iter()
            .filter(|p| p.abandoned && p.delivery == Delivery::Offered)
            .map(|p| p.id)
            .collect();
        for id in withdrawals {
            machine.apply(Event::NotSent(id)).unwrap();
        }
        assert!(!machine.pending_requests().any(|(_, op, _)| matches!(
            op,
            Operation::FreeBuffer(_) | Operation::AwaitCaller(Gate::Quiesce)
        )));
        respond(
            &mut machine,
            read,
            Reply::Failed(NcError::Stock(StockError::MemoryRead)),
        );
        assert_eq!(machine.state(), NcState::Inactive);
        assert_eq!(
            machine.error(),
            Some(NcError::Stock(StockError::MemoryRead))
        );
        let (quiesce, _) = find(&machine, |op| {
            matches!(op, Operation::AwaitCaller(Gate::Quiesce))
        });
        respond(
            &mut machine,
            quiesce,
            Reply::Fact(CallerFact::BackendQuiesced),
        );
        assert_eq!(
            machine
                .pending_requests()
                .filter(|(_, op, _)| matches!(op, Operation::FreeBuffer(_)))
                .count(),
            2
        );
        settle(&mut machine);
        assert!(machine.cleanup_complete());
        assert_eq!(machine.state(), NcState::Inactive);
    }

    #[test]
    fn failed_run_time_free_pauses_cleanup_before_quiescence_is_offered() {
        let requests = |effects: &[Effect]| -> Vec<Operation> {
            effects
                .iter()
                .filter_map(|effect| match effect {
                    Effect::Request { operation, .. } => Some(operation.clone()),
                    _ => None,
                })
                .collect()
        };
        for sent in [false, true] {
            let mut machine = calibrating();
            progress(&mut machine, 5.0);
            settle(&mut machine);
            let (delay, _) = find(&machine, |op| {
                matches!(op, Operation::RequestMeter { id: 5 })
            });
            respond(&mut machine, delay, Reply::BulkDelay(8.0));
            progress(&mut machine, 9.9);
            // Acknowledge state-9 entry work until only the run-time frees of 3/4 remain.
            loop {
                let next = machine
                    .pending_requests()
                    .find(|(_, op, _)| !matches!(op, Operation::FreeBuffer(_)))
                    .map(|(id, op, _)| (id, op.clone()));
                let Some((id, op)) = next else { break };
                acknowledge(&mut machine, id, op);
            }
            let frees: Vec<_> = machine.pending_requests().map(|(id, _, _)| id).collect();
            assert_eq!(frees.len(), 2);
            let leases = [BufferId::RefinerStimulus, BufferId::RefinerCapture]
                .map(|id| machine.buffer(id).expect("run-time lease"));

            let effects = if sent {
                machine.apply(Event::Sent(frees[0])).unwrap();
                machine
                    .apply(Event::Reply {
                        request: frees[0],
                        reply: Reply::Failed(NcError::Backend),
                    })
                    .unwrap()
            } else {
                machine.apply(Event::NotSent(frees[0])).unwrap()
            };
            assert_eq!(machine.state(), NcState::HandleError);
            assert!(requests(&effects).is_empty(), "paused cleanup offered work");
            // The other free was already offered: it keeps its id and is not re-issued.
            let survivor: Vec<_> = machine.pending_requests().map(|(id, _, _)| id).collect();
            assert_eq!(survivor, [frees[1]]);
            let effects = acknowledge(&mut machine, frees[1], Operation::FreeBuffer(leases[1]));
            assert!(requests(&effects).is_empty(), "paused cleanup offered work");
            assert_eq!(machine.buffer(BufferId::RefinerStimulus), Some(leases[0]));
            assert_eq!(
                machine.apply(Event::Command(Command::Open)),
                Err(Refusal::Busy)
            );

            let effects = machine
                .apply(Event::Command(Command::RetryCleanup))
                .unwrap();
            assert_eq!(requests(&effects), [Operation::AwaitCaller(Gate::Quiesce)]);
            settle(&mut machine);
            assert!(machine.cleanup_complete());
            assert!(BUFFER_IDS.iter().all(|&id| machine.buffer(id).is_none()));
            assert_eq!(machine.error(), Some(NcError::Backend));
        }
    }

    #[test]
    fn sanity_error_cleans_every_buffer_and_restart_preserves_16_to_1_to_6() {
        let mut machine = capturing();
        progress(&mut machine, 30.0);
        let (request, _) = find(&machine, |op| {
            matches!(op, Operation::RequestMeter { id: 6 })
        });
        respond(&mut machine, request, Reply::Sanity(5));
        assert_eq!(machine.state(), NcState::HandleError);
        assert_eq!(machine.error(), Some(NcError::Sanity(5)));
        assert_eq!(
            machine.apply(Event::Command(Command::Next)),
            Err(Refusal::Busy)
        );
        let effects = settle(&mut machine);
        let freed: Vec<_> = effects
            .iter()
            .filter_map(|effect| match effect {
                Effect::Request {
                    operation: Operation::FreeBuffer(lease),
                    ..
                } => Some(lease.id),
                _ => None,
            })
            .collect();
        assert_eq!(
            freed,
            vec![
                BufferId::Input,
                BufferId::Target,
                BufferId::TrainerParameters,
                BufferId::TrainerComms
            ]
        );
        assert_eq!(machine.error(), Some(NcError::Sanity(5)));
        assert!(machine.cleanup_complete());
        let effects = machine.apply(Event::Command(Command::Next)).unwrap();
        assert!(effects.contains(&Effect::Notify(Notice::State(NcState::Initial))));
        assert!(effects.contains(&Effect::Notify(Notice::State(NcState::MicSetup))));
        assert_eq!(machine.state(), NcState::MicSetup);
        assert!(machine.error().is_none());
    }

    #[test]
    fn every_restart_from_handled_error_restores_state_one_setup_before_calibration() {
        fn discard(machine: &mut CaptureOrchestrator) -> Vec<Effect> {
            machine.apply(Event::Command(Command::Back)).unwrap();
            let (decision, _) = find(machine, |op| {
                matches!(op, Operation::AwaitCaller(Gate::DiscardDecision))
            });
            respond(
                machine,
                decision,
                Reply::Fact(CallerFact::DiscardAllowed(true)),
            )
        }
        let triggers: [fn(&mut CaptureOrchestrator) -> Vec<Effect>; 4] = [
            |m| m.apply(Event::Command(Command::Open)).unwrap(),
            |m| m.apply(Event::Command(Command::Next)).unwrap(),
            |m| m.apply(Event::Command(Command::RemoteState(1))).unwrap(),
            discard,
        ];
        for trigger in triggers {
            let mut machine = capturing();
            progress(&mut machine, 30.0);
            let (sanity, _) = find(&machine, |op| {
                matches!(op, Operation::RequestMeter { id: 6 })
            });
            respond(&mut machine, sanity, Reply::Sanity(5));
            settle(&mut machine);
            assert!(machine.cleanup_complete());
            assert_eq!(machine.error(), Some(NcError::Sanity(5)));

            let states: Vec<_> = trigger(&mut machine)
                .into_iter()
                .filter_map(|effect| match effect {
                    Effect::Notify(Notice::State(state)) => Some(state),
                    _ => None,
                })
                .collect();
            assert_eq!(states, [NcState::Initial, NcState::MicSetup]);
            assert!(machine.error().is_none());
            let gates: Vec<_> = machine
                .pending_requests()
                .map(|(_, op, _)| op.clone())
                .collect();
            assert_eq!(
                gates,
                [
                    Operation::AwaitCaller(Gate::EntryServices(NcState::Initial)),
                    Operation::AwaitCaller(Gate::PlayerStarted),
                ]
            );
            for _ in 0..2 {
                let (id, op) = find(&machine, |_| true);
                assert_eq!(
                    machine.apply(Event::Command(Command::Next)),
                    Err(Refusal::Busy)
                );
                acknowledge(&mut machine, id, op);
            }
            let (writes, op) = find(&machine, |_| true);
            let p2 = ParamWrite {
                node: 1,
                parameter: 2,
                value: 1.0,
            };
            assert_eq!(op, Operation::SetParameters(vec![p2, p2]));
            machine.apply(Event::Sent(writes)).unwrap();
            assert_eq!(
                machine.apply(Event::Command(Command::Next)),
                Err(Refusal::Busy)
            );
            respond(&mut machine, writes, Reply::Executed);
            machine.apply(Event::Command(Command::Next)).unwrap();
            assert_eq!(machine.state(), NcState::Calibrate);
        }
    }

    #[test]
    fn identity_exhaustion_refuses_continuations_atomically_and_never_wraps() {
        let mut machine = calibrating();
        progress(&mut machine, 5.0);
        settle(&mut machine);
        let (delay, _) = find(&machine, |op| {
            matches!(op, Operation::RequestMeter { id: 5 })
        });
        respond(&mut machine, delay, Reply::BulkDelay(8.0));
        progress(&mut machine, 9.9);
        loop {
            let next = machine
                .pending_requests()
                .find(|(_, op, _)| !matches!(op, Operation::FreeBuffer(_)))
                .map(|(id, op, _)| (id, op.clone()));
            let Some((id, op)) = next else { break };
            acknowledge(&mut machine, id, op);
        }
        let frees: Vec<_> = machine
            .pending_requests()
            .filter_map(|(id, op, _)| match op {
                Operation::FreeBuffer(lease) => Some((id, *lease)),
                _ => None,
            })
            .collect();
        assert_eq!(frees.len(), 2);
        respond(&mut machine, frees[0].0, Reply::Freed(frees[0].1));

        // The last run-time free would number four allocations with two identifiers left.
        let (last, lease) = frees[1];
        machine.sequence = u64::MAX - 2;
        machine.apply(Event::Sent(last)).unwrap();
        let before = format!("{machine:?}");
        let freed = Event::Reply {
            request: last,
            reply: Reply::Freed(lease),
        };
        assert_eq!(
            machine.apply(freed.clone()),
            Err(Refusal::IdentityExhausted)
        );
        assert_eq!(format!("{machine:?}"), before);
        // The remaining headroom still drains the free and closes the run.
        machine.apply(Event::Command(Command::Close)).unwrap();
        machine.apply(freed).unwrap();
        settle(&mut machine);
        assert!(machine.cleanup_complete());
        assert_eq!(
            machine.apply(Event::Command(Command::Open)),
            Err(Refusal::IdentityExhausted)
        );

        // A draining 10 -> 11 reply at the last entry value refuses instead of wrapping.
        let mut machine = capturing();
        progress(&mut machine, 30.0);
        let (sanity, _) = find(&machine, |op| {
            matches!(op, Operation::RequestMeter { id: 6 })
        });
        respond(&mut machine, sanity, Reply::Sanity(0));
        let (read, op) = find(&machine, |op| matches!(op, Operation::ReadFile { .. }));
        let Operation::ReadFile { from, bytes, .. } = op else {
            unreachable!()
        };
        machine.epoch.entry = u64::MAX - 1;
        for pending in &mut machine.pending {
            pending.epoch = machine.epoch;
        }
        machine.apply(Event::Sent(read)).unwrap();
        let before = format!("{machine:?}");
        assert_eq!(
            machine.apply(Event::Reply {
                request: read,
                reply: Reply::TransferDone { lease: from, bytes }
            }),
            Err(Refusal::IdentityExhausted)
        );
        assert_eq!(format!("{machine:?}"), before);
        machine.epoch.generation = u64::MAX;
        let before = format!("{machine:?}");
        assert_eq!(
            machine.apply(Event::Command(Command::Close)),
            Err(Refusal::IdentityExhausted)
        );
        assert_eq!(format!("{machine:?}"), before);
    }

    #[test]
    fn wrong_id_lease_unsent_and_nonfinite_replies_never_advance() {
        let mut machine = machine();
        machine.apply(Event::Command(Command::Open)).unwrap();
        let (request, _) = find(&machine, |op| {
            matches!(op, Operation::AwaitCaller(Gate::PlayerStarted))
        });
        assert_eq!(
            machine.apply(Event::Reply {
                request,
                reply: Reply::Fact(CallerFact::PlayerStarted)
            }),
            Err(Refusal::NotSent)
        );
        settle(&mut machine);
        machine
            .apply(Event::Command(Command::RemoteState(6)))
            .unwrap();
        assert_eq!(
            machine.apply(Event::Command(Command::RemoteState(7))),
            Err(Refusal::Unsupported)
        );
        machine.apply(Event::Command(Command::Next)).unwrap();
        let (request, op) = find(&machine, |op| {
            matches!(
                op,
                Operation::Allocate {
                    id: BufferId::RefinerStimulus,
                    ..
                }
            )
        });
        machine.apply(Event::Sent(request)).unwrap();
        assert_eq!(
            machine.apply(Event::Reply {
                request,
                reply: Reply::Allocated {
                    id: BufferId::Input,
                    bytes: 96,
                    address: 0x1000
                }
            }),
            Err(Refusal::WrongReply)
        );
        acknowledge(&mut machine, request, op);
        settle(&mut machine);
        assert_eq!(
            machine.apply(Event::Progress {
                epoch: machine.epoch(),
                value: f32::INFINITY
            }),
            Err(Refusal::NonFinite)
        );
        progress(&mut machine, 5.0);
        settle(&mut machine);
        let (request, _) = find(&machine, |op| {
            matches!(op, Operation::RequestMeter { id: 5 })
        });
        machine.apply(Event::Sent(request)).unwrap();
        assert_eq!(
            machine.apply(Event::Reply {
                request,
                reply: Reply::BulkDelay(f32::NAN)
            }),
            Err(Refusal::NonFinite)
        );
        respond(&mut machine, request, Reply::BulkDelay(2.0));
        progress(&mut machine, 9.9);
        let (request, op) = find(&machine, |op| matches!(op, Operation::ReadFile { .. }));
        let Operation::ReadFile {
            from: lease, bytes, ..
        } = op
        else {
            unreachable!()
        };
        machine.apply(Event::Sent(request)).unwrap();
        let wrong = BufferLease {
            allocation: RequestId {
                sequence: lease.allocation.sequence + 1,
                ..lease.allocation
            },
            ..lease
        };
        assert_eq!(
            machine.apply(Event::Reply {
                request,
                reply: Reply::TransferDone {
                    lease: wrong,
                    bytes
                }
            }),
            Err(Refusal::WrongReply)
        );
        assert_eq!(machine.state(), NcState::Capturing);
        assert!(machine.buffer(BufferId::RefinerCapture).is_some());
    }

    #[test]
    fn identity_headroom_blocks_new_work_but_still_drains_cleanup_replies() {
        let mut machine = calibrating();
        machine.sequence = u64::MAX - REQUEST_LIMIT as u64 + 1;
        machine.epoch.entry = u64::MAX - REQUEST_LIMIT as u64 + 1;
        machine.epoch.generation = u64::MAX - REQUEST_LIMIT as u64 + 1;
        assert_eq!(
            machine.apply(Event::Progress {
                epoch: machine.epoch(),
                value: 5.0
            }),
            Err(Refusal::IdentityExhausted)
        );
        machine.apply(Event::Command(Command::Close)).unwrap();
        settle(&mut machine);
        assert!(machine.cleanup_complete());
        assert_eq!(
            machine.apply(Event::Command(Command::Open)),
            Err(Refusal::IdentityExhausted)
        );
    }

    #[test]
    fn headroom_refuses_discard_restart_and_reentry_but_close_still_drains() {
        fn discard(machine: &mut CaptureOrchestrator) -> Event {
            machine.apply(Event::Command(Command::Back)).unwrap();
            let (request, _) = find(machine, |op| {
                matches!(op, Operation::AwaitCaller(Gate::DiscardDecision))
            });
            machine.apply(Event::Sent(request)).unwrap();
            Event::Reply {
                request,
                reply: Reply::Fact(CallerFact::DiscardAllowed(true)),
            }
        }

        // Restart: Back in a cleaned 16 at the last admitted value numbers the decision
        // inside headroom; its accepted reply must not start the 1 -> 6 restoration.
        let mut machine = capturing();
        progress(&mut machine, 30.0);
        let (sanity, _) = find(&machine, |op| {
            matches!(op, Operation::RequestMeter { id: 6 })
        });
        respond(&mut machine, sanity, Reply::Sanity(5));
        settle(&mut machine);
        machine.sequence = u64::MAX - REQUEST_LIMIT as u64;
        let allowed = discard(&mut machine);
        let before = format!("{machine:?}");
        assert_eq!(
            machine.apply(allowed.clone()),
            Err(Refusal::IdentityExhausted)
        );
        assert_eq!(format!("{machine:?}"), before);
        machine.apply(Event::Command(Command::Close)).unwrap();
        machine.apply(allowed).unwrap();
        settle(&mut machine);
        assert!(machine.cleanup_complete());
        assert_eq!(machine.state(), NcState::Inactive);
        assert_eq!(machine.error(), Some(NcError::Sanity(5)));

        // Automatic re-entry: the final cleanup Freed after a discard must not enter 1.
        let mut machine = capturing();
        let allowed = discard(&mut machine);
        machine.apply(allowed).unwrap();
        machine.sequence = u64::MAX - REQUEST_LIMIT as u64;
        let (last, lease) = loop {
            let (id, op) = find(&machine, |_| true);
            if let Operation::FreeBuffer(lease) = op
                && machine.pending_requests().count() == 1
            {
                break (id, lease);
            }
            acknowledge(&mut machine, id, op);
        };
        machine.apply(Event::Sent(last)).unwrap();
        let freed = Event::Reply {
            request: last,
            reply: Reply::Freed(lease),
        };
        let before = format!("{machine:?}");
        assert_eq!(
            machine.apply(freed.clone()),
            Err(Refusal::IdentityExhausted)
        );
        assert_eq!(format!("{machine:?}"), before);
        assert_eq!(machine.buffer(lease.id), Some(lease));
        // Close changes the cleanup target to 0; the same reply then drains without a run.
        machine.apply(Event::Command(Command::Close)).unwrap();
        let effects = machine.apply(freed).unwrap();
        assert!(effects.is_empty());
        assert!(machine.cleanup_complete());
        assert_eq!(machine.state(), NcState::Inactive);
        assert_eq!(
            machine.apply(Event::Command(Command::Open)),
            Err(Refusal::IdentityExhausted)
        );
    }

    #[test]
    fn queued_start_after_cancel_is_drained_before_quiescence_is_requested() {
        let mut machine = capturing();
        progress(&mut machine, 30.0);
        let (request, _) = find(&machine, |op| {
            matches!(op, Operation::RequestMeter { id: 6 })
        });
        respond(&mut machine, request, Reply::Sanity(0));
        let start = loop {
            let (id, op) = find(&machine, |_| true);
            if matches!(op, Operation::SetParameters(_)) {
                break id;
            }
            acknowledge(&mut machine, id, op);
        };
        let effects = machine.apply(Event::Command(Command::Close)).unwrap();
        assert!(effects.contains(&Effect::Withdraw(start)));
        assert!(
            !machine
                .pending_requests()
                .any(|(_, op, _)| matches!(op, Operation::AwaitCaller(Gate::Quiesce)))
        );
        machine.apply(Event::Sent(start)).unwrap();
        assert_eq!(machine.state(), NcState::Inactive);
        respond(&mut machine, start, Reply::Executed);
        assert_eq!(machine.state(), NcState::Inactive);
        assert!(
            !machine
                .pending_requests()
                .any(|(_, op, _)| matches!(op, Operation::FreeBuffer(_)))
        );
        settle(&mut machine);
        assert!(machine.cleanup_complete());
    }

    #[test]
    fn allocation_mismatch_is_retained_for_cleanup_and_unresolved_services_fail_closed() {
        assert!(matches!(
            CaptureOrchestrator::new(Sizes {
                input_bytes: 3,
                refine_bytes: 96
            }),
            Err(Refusal::InvalidSize)
        ));
        let mut machine = machine();
        machine.apply(Event::Command(Command::Open)).unwrap();
        settle(&mut machine);
        machine
            .apply(Event::Command(Command::RemoteState(6)))
            .unwrap();
        machine.apply(Event::Command(Command::Next)).unwrap();
        let (request, _) = find(&machine, |op| {
            matches!(
                op,
                Operation::Allocate {
                    id: BufferId::RefinerCapture,
                    ..
                }
            )
        });
        respond(
            &mut machine,
            request,
            Reply::Allocated {
                id: BufferId::RefinerCapture,
                bytes: 92,
                address: 0x1000,
            },
        );
        assert_eq!(machine.error(), Some(NcError::AllocationMismatch));
        assert_eq!(machine.buffer(BufferId::RefinerCapture).unwrap().bytes, 92);
        settle(&mut machine);
        assert!(machine.cleanup_complete());
        machine.apply(Event::Command(Command::Next)).unwrap();
        settle(&mut machine);
        machine.apply(Event::Command(Command::Next)).unwrap();
        let (services, _) = find(&machine, |op| {
            matches!(
                op,
                Operation::AwaitCaller(Gate::EntryServices(NcState::Calibrate))
            )
        });
        respond(
            &mut machine,
            services,
            Reply::Failed(NcError::UnsupportedService),
        );
        assert_eq!(machine.state(), NcState::HandleError);
        assert!(!machine.pending_requests().any(|(_, op, _)| matches!(
            op,
            Operation::WriteFile { .. } | Operation::SetParameters(_)
        )));
        settle(&mut machine);
        assert!(machine.cleanup_complete());
    }

    #[test]
    fn partial_parameter_failure_retains_allocations_and_requires_quiescence() {
        let mut machine = capturing();
        progress(&mut machine, 30.0);
        let (request, _) = find(&machine, |op| {
            matches!(op, Operation::RequestMeter { id: 6 })
        });
        respond(&mut machine, request, Reply::Sanity(0));
        loop {
            let (id, op) = find(&machine, |_| true);
            if matches!(op, Operation::SetParameters(_)) {
                machine.apply(Event::Sent(id)).unwrap();
                respond(&mut machine, id, Reply::Failed(NcError::Backend));
                break;
            }
            acknowledge(&mut machine, id, op);
        }
        assert_eq!(machine.state(), NcState::HandleError);
        assert_eq!(machine.buffers.iter().filter(|b| b.is_some()).count(), 4);
        assert!(
            !machine
                .pending_requests()
                .any(|(_, op, _)| matches!(op, Operation::FreeBuffer(_)))
        );
        let (quiesce, _) = find(&machine, |op| {
            matches!(op, Operation::AwaitCaller(Gate::Quiesce))
        });
        respond(
            &mut machine,
            quiesce,
            Reply::Failed(NcError::UnsupportedService),
        );
        assert!(!machine.cleanup_complete());
        machine
            .apply(Event::Command(Command::RetryCleanup))
            .unwrap();
        settle(&mut machine);
        assert!(machine.cleanup_complete());
        assert_eq!(machine.error(), Some(NcError::UnsupportedService));
    }
}

//! Owned single-core service protocol, not a stock ABI or physical resident.
//! All ordering is logical: completion accepts input, processing publishes output,
//! and transmit completion releases ownership. No cache/coherency claim is made.
//! Control errors leave state unchanged. Work-event errors fault a ready model
//! before operation commits, preserving ownership until explicit quiescence.
//! The crate README specifies the full rollback, numeric and recovery policies.

pub mod audio_port;

use crate::gain_backend::{EmittedGainBackend, InstructionBudget, RawExecutionError};
use tetrad_sharc::simulator::SimulationOutcome;

pub const BLOCK_FRAMES: usize = 32;
pub const CHANNELS: usize = 1;
pub const BLOCK_SLOTS: usize = 2;
pub const CONTEXT_SLOTS: usize = 2;
pub type MonoBlock = [f32; BLOCK_FRAMES];
static SILENCE: MonoBlock = [0.0; BLOCK_FRAMES];

macro_rules! counter {
    ($name:ident) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
        pub struct $name(u64);
        impl $name {
            pub const fn new(value: u64) -> Self {
                Self(value)
            }
            pub const fn get(self) -> u64 {
                self.0
            }
        }
    };
}
counter!(Epoch);
counter!(BlockGeneration);
counter!(ContextGeneration);
counter!(ParameterGeneration);
counter!(Sequence);
counter!(Tick);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockSlot {
    First,
    Second,
}
impl BlockSlot {
    const fn index(self) -> usize {
        match self {
            Self::First => 0,
            Self::Second => 1,
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContextSlot {
    First,
    Second,
}
impl ContextSlot {
    const fn index(self) -> usize {
        match self {
            Self::First => 0,
            Self::Second => 1,
        }
    }
}

/// Acceptance of an owned epoch. This does not report a hardware ready message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReadyToken {
    epoch: Epoch,
}
impl ReadyToken {
    pub const fn epoch(self) -> Epoch {
        self.epoch
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContextHandle {
    epoch: Epoch,
    slot: ContextSlot,
    generation: ContextGeneration,
}
impl ContextHandle {
    pub const fn epoch(self) -> Epoch {
        self.epoch
    }
    pub const fn slot(self) -> ContextSlot {
        self.slot
    }
    pub const fn generation(self) -> ContextGeneration {
        self.generation
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlockHandle {
    epoch: Epoch,
    slot: BlockSlot,
    generation: BlockGeneration,
}
impl BlockHandle {
    pub const fn epoch(self) -> Epoch {
        self.epoch
    }
    pub const fn slot(self) -> BlockSlot {
        self.slot
    }
    pub const fn generation(self) -> BlockGeneration {
        self.generation
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Receive,
    Transmit,
}
/// Injectable completion record. The receiving method checks its direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Completion {
    pub direction: Direction,
    pub epoch: Epoch,
    pub block: BlockHandle,
    pub sequence: Sequence,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ownership {
    Free,
    DmaReceive,
    DspReady,
    Processing,
    TransmitReady,
    DmaTransmit,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PhysicalOperation {
    Backend,
    Start,
    Load,
    Audio,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MissingProof {
    MemoryMap,
    BootHandoff,
    CacheOrdering,
    Interrupts,
    AudioRouting,
    SecondSoc,
}
pub const PHYSICAL_GATES: [MissingProof; 6] = [
    MissingProof::MemoryMap,
    MissingProof::BootHandoff,
    MissingProof::CacheOrdering,
    MissingProof::Interrupts,
    MissingProof::AudioRouting,
    MissingProof::SecondSoc,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    #[error("operation requires an owned ready epoch")]
    NotReady,
    #[error("re-arm requires explicit quiescence acknowledgement")]
    QuiescenceRequired,
    #[error("epoch {proposed:?} must be nonzero and newer than {previous:?}")]
    EpochNotFresh { previous: Epoch, proposed: Epoch },
    #[error("stale epoch: expected {expected:?}, got {received:?}")]
    StaleEpoch { expected: Epoch, received: Epoch },
    #[error("stale context handle")]
    StaleContext,
    #[error("stale block handle")]
    StaleBlock,
    #[error("static context slot is already reserved")]
    ContextReserved,
    #[error("context still owns a block")]
    ContextBusy,
    #[error("single-core processor already owns a block")]
    ProcessorBusy,
    #[error("block overrun: static slot is not free")]
    Overrun,
    #[error("invalid ownership: expected {expected:?}, got {actual:?}")]
    Ownership {
        expected: Ownership,
        actual: Ownership,
    },
    #[error("{direction:?} sequence: expected {expected:?}, got {received:?}")]
    Sequence {
        direction: Direction,
        expected: Sequence,
        received: Sequence,
    },
    #[error("completion direction: expected {expected:?}, got {received:?}")]
    CompletionDirection {
        expected: Direction,
        received: Direction,
    },
    #[error("checked counter exhausted: {0}")]
    Exhausted(&'static str),
    #[error("parameter generation: expected {expected:?}, got {received:?}")]
    ParameterGeneration {
        expected: ParameterGeneration,
        received: ParameterGeneration,
    },
    #[error("gain must be finite")]
    NonFiniteGain,
    #[error("nonfinite input at frame {0}")]
    NonFiniteInput(usize),
    #[error("nonfinite multiplication result at frame {0}")]
    NonFiniteOutput(usize),
    #[error(transparent)]
    RawExecution(#[from] RawExecutionError),
    #[error("clock rollback from {previous:?} to {now:?}")]
    ClockRollback { previous: Tick, now: Tick },
    #[error("deadline arithmetic overflow")]
    DeadlineOverflow,
    #[error("processing finished at {now:?} after deadline {deadline:?}")]
    Deadline { deadline: Tick, now: Tick },
    #[error("invalid processing token")]
    ProcessingToken,
    #[error("physical {0:?} unavailable; consult PHYSICAL_GATES")]
    PhysicalUnavailable(PhysicalOperation),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lifecycle {
    Quiescent,
    Ready { epoch: Epoch },
    Fault { epoch: Epoch, cause: Error },
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Status {
    pub lifecycle: Lifecycle,
    pub blocks: [Ownership; BLOCK_SLOTS],
    pub contexts: [bool; CONTEXT_SLOTS],
}
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Parameters {
    pub generation: ParameterGeneration,
    pub gain: f32,
}
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ContextStatus {
    pub active: Parameters,
    pub pending: Option<Parameters>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProcessingToken {
    block: BlockHandle,
    parameter_generation: ParameterGeneration,
    start: Tick,
    deadline: Tick,
}
impl ProcessingToken {
    pub const fn block(self) -> BlockHandle {
        self.block
    }
    pub const fn parameter_generation(self) -> ParameterGeneration {
        self.parameter_generation
    }
    pub const fn deadline(self) -> Tick {
        self.deadline
    }
}

/// Receipt from the owned raw-word simulator, after atomic output publication.
/// Steps/stores are mathematical decoder events, not physical SHARC timing/DMA.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExecutionReceipt {
    pub block: BlockHandle,
    pub execution: SimulationOutcome,
}

#[derive(Debug, Clone, Copy)]
struct Context {
    generation: ContextGeneration,
    parameters: Option<ContextStatus>,
}
#[derive(Debug, Clone, Copy)]
struct Block {
    generation: BlockGeneration,
    ownership: Ownership,
    context: Option<ContextHandle>,
    sequence: Sequence,
    processing: Option<ProcessingToken>,
    gain: f32,
    input: MonoBlock,
    output: MonoBlock,
}
impl Block {
    const fn empty(generation: BlockGeneration) -> Self {
        Self {
            generation,
            ownership: Ownership::Free,
            context: None,
            sequence: Sequence(0),
            processing: None,
            gain: 0.0,
            input: [0.0; BLOCK_FRAMES],
            output: [0.0; BLOCK_FRAMES],
        }
    }
}

/// Resident storage is fixed; the opt-in emitted simulator allocates scratch.
/// Epochs must be unique within the caller's model/session; tokens must not be
/// transferred to a different Resident instance.
#[derive(Debug)]
pub struct Resident {
    lifecycle: Lifecycle,
    last_epoch: Epoch,
    last_tick: Tick,
    contexts: [Context; CONTEXT_SLOTS],
    blocks: [Block; BLOCK_SLOTS],
    receive_issue: Sequence,
    receive_complete: Sequence,
    transmit_issue: Sequence,
    transmit_complete: Sequence,
}
impl Default for Resident {
    fn default() -> Self {
        Self::new()
    }
}
impl Resident {
    pub const fn new() -> Self {
        Self {
            lifecycle: Lifecycle::Quiescent,
            last_epoch: Epoch(0),
            last_tick: Tick(0),
            contexts: [Context {
                generation: ContextGeneration(0),
                parameters: None,
            }; CONTEXT_SLOTS],
            blocks: [Block::empty(BlockGeneration(0)); BLOCK_SLOTS],
            receive_issue: Sequence(1),
            receive_complete: Sequence(1),
            transmit_issue: Sequence(1),
            transmit_complete: Sequence(1),
        }
    }
    pub fn status(&self) -> Status {
        Status {
            lifecycle: self.lifecycle,
            blocks: self.blocks.map(|b| b.ownership),
            contexts: self.contexts.map(|c| c.parameters.is_some()),
        }
    }
    pub fn physical(&self, operation: PhysicalOperation) -> Result<(), Error> {
        Err(Error::PhysicalUnavailable(operation))
    }
    pub fn arm(&mut self, epoch: Epoch, now: Tick) -> Result<ReadyToken, Error> {
        if self.lifecycle != Lifecycle::Quiescent {
            return Err(Error::QuiescenceRequired);
        }
        if epoch <= self.last_epoch {
            return Err(Error::EpochNotFresh {
                previous: self.last_epoch,
                proposed: epoch,
            });
        }
        self.check_tick(now)?;
        self.last_epoch = epoch;
        self.last_tick = now;
        self.lifecycle = Lifecycle::Ready { epoch };
        self.receive_issue = Sequence(1);
        self.receive_complete = Sequence(1);
        self.transmit_issue = Sequence(1);
        self.transmit_complete = Sequence(1);
        Ok(ReadyToken { epoch })
    }
    /// Caller assertion that all modeled transfers have stopped. Not a hardware ack.
    /// This is the only operation allowed to release buffers following a fault.
    pub fn acknowledge_quiescence(&mut self, ready: ReadyToken) -> Result<(), Error> {
        self.check_epoch(ready.epoch)?;
        for block in &mut self.blocks {
            *block = Block::empty(block.generation);
        }
        for context in &mut self.contexts {
            context.parameters = None;
        }
        self.lifecycle = Lifecycle::Quiescent;
        Ok(())
    }
    /// Separate immutable owned zeros, never a statement that DMA sent silence.
    pub fn fault_silence(&self) -> Option<&'static MonoBlock> {
        matches!(self.lifecycle, Lifecycle::Fault { .. }).then_some(&SILENCE)
    }
    pub fn create_context(
        &mut self,
        ready: ReadyToken,
        slot: ContextSlot,
        gain: f32,
    ) -> Result<ContextHandle, Error> {
        self.check_ready(ready.epoch)?;
        if !gain.is_finite() {
            return Err(Error::NonFiniteGain);
        }
        let context = &mut self.contexts[slot.index()];
        if context.parameters.is_some() {
            return Err(Error::ContextReserved);
        }
        let generation = ContextGeneration(next(context.generation.0, "context generation")?);
        context.generation = generation;
        context.parameters = Some(ContextStatus {
            active: Parameters {
                generation: ParameterGeneration(1),
                gain,
            },
            pending: None,
        });
        Ok(ContextHandle {
            epoch: ready.epoch,
            slot,
            generation,
        })
    }
    pub fn context_status(&self, handle: ContextHandle) -> Result<ContextStatus, Error> {
        self.check_ready(handle.epoch)?;
        let index = self.context_index(handle)?;
        self.contexts[index].parameters.ok_or(Error::StaleContext)
    }
    pub fn reset_context(
        &mut self,
        handle: ContextHandle,
        gain: f32,
    ) -> Result<ContextHandle, Error> {
        self.check_ready(handle.epoch)?;
        let index = self.context_index(handle)?;
        self.check_context_idle(handle)?;
        if !gain.is_finite() {
            return Err(Error::NonFiniteGain);
        }
        let generation = ContextGeneration(next(
            self.contexts[index].generation.0,
            "context generation",
        )?);
        self.contexts[index] = Context {
            generation,
            parameters: Some(ContextStatus {
                active: Parameters {
                    generation: ParameterGeneration(1),
                    gain,
                },
                pending: None,
            }),
        };
        Ok(ContextHandle {
            generation,
            ..handle
        })
    }
    pub fn destroy_context(&mut self, handle: ContextHandle) -> Result<(), Error> {
        self.check_ready(handle.epoch)?;
        let index = self.context_index(handle)?;
        self.check_context_idle(handle)?;
        self.contexts[index].parameters = None;
        Ok(())
    }
    /// Contiguous generations only. A newer update may replace a pending update.
    /// Activation occurs only at begin_processing, never during a block.
    pub fn update_gain(
        &mut self,
        handle: ContextHandle,
        generation: ParameterGeneration,
        gain: f32,
    ) -> Result<(), Error> {
        self.check_ready(handle.epoch)?;
        let index = self.context_index(handle)?;
        if !gain.is_finite() {
            return Err(Error::NonFiniteGain);
        }
        let parameters = self.contexts[index]
            .parameters
            .as_mut()
            .ok_or(Error::StaleContext)?;
        let previous = parameters.pending.unwrap_or(parameters.active).generation;
        let expected = ParameterGeneration(next(previous.0, "parameter generation")?);
        if generation != expected {
            return Err(Error::ParameterGeneration {
                expected,
                received: generation,
            });
        }
        parameters.pending = Some(Parameters { generation, gain });
        Ok(())
    }

    pub fn reserve_receive(
        &mut self,
        ready: ReadyToken,
        context: ContextHandle,
        slot: BlockSlot,
    ) -> Result<Completion, Error> {
        let result = (|| {
            self.check_ready(ready.epoch)?;
            self.context_index(context)?;
            let index = slot.index();
            if self.blocks[index].ownership != Ownership::Free {
                return Err(Error::Overrun);
            }
            let generation =
                BlockGeneration(next(self.blocks[index].generation.0, "block generation")?);
            let sequence = self.receive_issue;
            let following = Sequence(next(sequence.0, "receive sequence")?);
            let handle = BlockHandle {
                epoch: ready.epoch,
                slot,
                generation,
            };
            self.blocks[index] = Block {
                ownership: Ownership::DmaReceive,
                context: Some(context),
                sequence,
                ..Block::empty(generation)
            };
            self.receive_issue = following;
            Ok(Completion {
                direction: Direction::Receive,
                epoch: ready.epoch,
                block: handle,
                sequence,
            })
        })();
        self.event(result)
    }
    pub fn complete_receive(
        &mut self,
        completion: Completion,
        input: &MonoBlock,
    ) -> Result<BlockHandle, Error> {
        let result = (|| {
            let (index, following) =
                self.validate_completion(completion, Direction::Receive, Ownership::DmaReceive)?;
            for (frame, sample) in input.iter().enumerate() {
                if !sample.is_finite() {
                    return Err(Error::NonFiniteInput(frame));
                }
            }
            self.blocks[index].input = *input;
            self.blocks[index].ownership = Ownership::DspReady;
            self.receive_complete = following;
            Ok(completion.block)
        })();
        self.event(result)
    }
    pub fn begin_processing(
        &mut self,
        handle: BlockHandle,
        now: Tick,
        budget_ticks: u64,
    ) -> Result<ProcessingToken, Error> {
        let result = (|| {
            self.check_ready(handle.epoch)?;
            let index = self.block_index(handle)?;
            self.check_ownership(index, Ownership::DspReady)?;
            self.check_tick(now)?;
            let deadline = Tick(
                now.0
                    .checked_add(budget_ticks)
                    .ok_or(Error::DeadlineOverflow)?,
            );
            let context = self.blocks[index].context.ok_or(Error::StaleContext)?;
            let ci = self.context_index(context)?;
            if self
                .blocks
                .iter()
                .any(|b| b.ownership == Ownership::Processing)
            {
                return Err(Error::ProcessorBusy);
            }
            let parameters = self.contexts[ci]
                .parameters
                .as_mut()
                .ok_or(Error::StaleContext)?;
            if let Some(pending) = parameters.pending.take() {
                parameters.active = pending;
            }
            let token = ProcessingToken {
                block: handle,
                parameter_generation: parameters.active.generation,
                start: now,
                deadline,
            };
            self.blocks[index].gain = parameters.active.gain;
            self.blocks[index].processing = Some(token);
            self.blocks[index].ownership = Ownership::Processing;
            self.last_tick = now;
            Ok(token)
        })();
        self.event(result)
    }
    /// Atomic reference f32 multiplication into fixed temporary storage. No host
    /// elapsed time, hardware cycles or SHARC exceptional arithmetic are inferred.
    pub fn finish_processing(
        &mut self,
        token: ProcessingToken,
        now: Tick,
    ) -> Result<BlockHandle, Error> {
        self.finish_with(token, now, |input, gain| {
            Ok((input.map(|sample| sample * gain), token.block))
        })
    }
    /// Opt-in owned gain execution. Input/gain come only from the validated
    /// resident block frozen at begin_processing. The backend owns immutable
    /// generated code; no arbitrary callback can produce an execution receipt.
    pub fn finish_processing_emitted(
        &mut self,
        token: ProcessingToken,
        now: Tick,
        backend: &EmittedGainBackend,
        budget: InstructionBudget,
    ) -> Result<ExecutionReceipt, Error> {
        self.finish_with(token, now, |input, gain| {
            let (output, execution) = backend.execute(input, gain, budget)?;
            Ok((
                output,
                ExecutionReceipt {
                    block: token.block,
                    execution,
                },
            ))
        })
    }
    // Private only: both public finishes share all lifecycle checks, finite
    // validation, publication and fault handling. Callers cannot supply output.
    fn finish_with<T>(
        &mut self,
        token: ProcessingToken,
        now: Tick,
        execute: impl FnOnce(&MonoBlock, f32) -> Result<(MonoBlock, T), Error>,
    ) -> Result<T, Error> {
        let result = (|| {
            self.check_ready(token.block.epoch)?;
            let index = self.block_index(token.block)?;
            self.check_ownership(index, Ownership::Processing)?;
            if self.blocks[index].processing != Some(token) {
                return Err(Error::ProcessingToken);
            }
            self.check_tick(now)?;
            if now > token.deadline {
                return Err(Error::Deadline {
                    deadline: token.deadline,
                    now,
                });
            }
            let (output, receipt) = execute(&self.blocks[index].input, self.blocks[index].gain)?;
            for (frame, sample) in output.iter().enumerate() {
                if !sample.is_finite() {
                    return Err(Error::NonFiniteOutput(frame));
                }
            }
            self.blocks[index].output = output;
            self.blocks[index].ownership = Ownership::TransmitReady;
            self.blocks[index].processing = None;
            self.last_tick = now;
            Ok(receipt)
        })();
        self.event(result)
    }
    pub fn begin_transmit(&mut self, handle: BlockHandle) -> Result<Completion, Error> {
        let result = (|| {
            self.check_ready(handle.epoch)?;
            let index = self.block_index(handle)?;
            self.check_ownership(index, Ownership::TransmitReady)?;
            let sequence = self.transmit_issue;
            let following = Sequence(next(sequence.0, "transmit sequence")?);
            self.blocks[index].sequence = sequence;
            self.blocks[index].ownership = Ownership::DmaTransmit;
            self.transmit_issue = following;
            Ok(Completion {
                direction: Direction::Transmit,
                epoch: handle.epoch,
                block: handle,
                sequence,
            })
        })();
        self.event(result)
    }
    pub fn complete_transmit(&mut self, completion: Completion) -> Result<(), Error> {
        let result = (|| {
            let (index, following) =
                self.validate_completion(completion, Direction::Transmit, Ownership::DmaTransmit)?;
            self.blocks[index] = Block::empty(self.blocks[index].generation);
            self.transmit_complete = following;
            Ok(())
        })();
        self.event(result)
    }
    /// In fault, only a transmit-owned buffer remains readable for inspection.
    /// Other output cannot be submitted again; no mutable buffer access is exposed.
    pub fn transmit_samples(&self, handle: BlockHandle) -> Result<&MonoBlock, Error> {
        let index = self.block_index(handle)?;
        let ownership = self.blocks[index].ownership;
        match self.lifecycle {
            Lifecycle::Ready { .. }
                if matches!(ownership, Ownership::TransmitReady | Ownership::DmaTransmit) =>
            {
                Ok(&self.blocks[index].output)
            }
            Lifecycle::Fault { .. } if ownership == Ownership::DmaTransmit => {
                Ok(&self.blocks[index].output)
            }
            _ => Err(Error::NotReady),
        }
    }

    fn event<T>(&mut self, result: Result<T, Error>) -> Result<T, Error> {
        if let (Err(cause), Lifecycle::Ready { epoch }) = (&result, self.lifecycle) {
            self.lifecycle = Lifecycle::Fault {
                epoch,
                cause: *cause,
            };
        }
        result
    }
    fn check_epoch(&self, received: Epoch) -> Result<(), Error> {
        let expected = match self.lifecycle {
            Lifecycle::Ready { epoch } | Lifecycle::Fault { epoch, .. } => epoch,
            Lifecycle::Quiescent => return Err(Error::NotReady),
        };
        if received != expected {
            return Err(Error::StaleEpoch { expected, received });
        }
        Ok(())
    }
    fn check_ready(&self, epoch: Epoch) -> Result<(), Error> {
        self.check_epoch(epoch)?;
        if !matches!(self.lifecycle, Lifecycle::Ready { .. }) {
            return Err(Error::NotReady);
        }
        Ok(())
    }
    fn context_index(&self, handle: ContextHandle) -> Result<usize, Error> {
        self.check_epoch(handle.epoch)?;
        let index = handle.slot.index();
        if self.contexts[index].generation != handle.generation
            || self.contexts[index].parameters.is_none()
        {
            return Err(Error::StaleContext);
        }
        Ok(index)
    }
    fn check_context_idle(&self, handle: ContextHandle) -> Result<(), Error> {
        if self.blocks.iter().any(|b| b.context == Some(handle)) {
            return Err(Error::ContextBusy);
        }
        Ok(())
    }
    fn block_index(&self, handle: BlockHandle) -> Result<usize, Error> {
        self.check_epoch(handle.epoch)?;
        let index = handle.slot.index();
        if self.blocks[index].generation != handle.generation {
            return Err(Error::StaleBlock);
        }
        Ok(index)
    }
    fn check_ownership(&self, index: usize, expected: Ownership) -> Result<(), Error> {
        let actual = self.blocks[index].ownership;
        if actual != expected {
            return Err(Error::Ownership { expected, actual });
        }
        Ok(())
    }
    fn check_tick(&self, now: Tick) -> Result<(), Error> {
        if now < self.last_tick {
            return Err(Error::ClockRollback {
                previous: self.last_tick,
                now,
            });
        }
        Ok(())
    }
    fn validate_completion(
        &self,
        completion: Completion,
        direction: Direction,
        ownership: Ownership,
    ) -> Result<(usize, Sequence), Error> {
        self.check_ready(completion.epoch)?;
        if completion.direction != direction {
            return Err(Error::CompletionDirection {
                expected: direction,
                received: completion.direction,
            });
        }
        let index = self.block_index(completion.block)?;
        let expected = match direction {
            Direction::Receive => self.receive_complete,
            Direction::Transmit => self.transmit_complete,
        };
        if completion.sequence != expected {
            return Err(Error::Sequence {
                direction,
                expected,
                received: completion.sequence,
            });
        }
        let following = Sequence(next(expected.0, "completion sequence")?);
        self.check_ownership(index, ownership)?;
        if self.blocks[index].sequence != completion.sequence {
            return Err(Error::Sequence {
                direction,
                expected: self.blocks[index].sequence,
                received: completion.sequence,
            });
        }
        Ok((index, following))
    }
}
fn next(value: u64, name: &'static str) -> Result<u64, Error> {
    value.checked_add(1).ok_or(Error::Exhausted(name))
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod emitted_tests;

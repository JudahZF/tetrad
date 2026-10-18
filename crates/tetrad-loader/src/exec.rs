//! Runs a [`BootPlan`] over injected hardware. Deterministic for a deterministic backend:
//! all time comes from [`PhysicalMemory::now`] and all waits are bounded.
use std::io;
use std::time::Duration;

use thiserror::Error;

use crate::PhysAddr;
use crate::booter::{AppMailbox, CoreMap, Region, Source, StreamMailbox, Swap};
use crate::intent::{CoreAction, LoaderCore};
use crate::plan::{BootPlan, Soc1Plan, Step};

/// loadsharc gives up when `difftime(time(), start) > 8.0` while waiting for the SHARC.
/// `time()` has 1 s resolution, so stock gives up after 8 to 9 s; this uses the upper edge.
pub const MAILBOX_TIMEOUT: Duration = Duration::from_secs(9);
/// Poll interval while waiting for the kernel to report start-up (`usleep(500000)`).
pub const APP_START_POLL: Duration = Duration::from_millis(500);

/// Mailbox words of the loadsharc protocol.
pub mod word {
    pub const ARM_READY: u32 = 0xdead;
    pub const BOOTER_READY: u32 = 0xb0b0;
    pub const REQUEST: u32 = 0x5555;
    pub const GRANT: u32 = 0xbbbb;
    pub const DONE: u32 = 0x6666;
    /// SHARC request count meaning "stop streaming".
    pub const STOP_COUNT: u32 = u32::MAX;
    pub const REFUSED: u32 = 0xbeef;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum AccessError {
    #[error("physical address {0} is outside every mapped window")]
    Unmapped(PhysAddr),
    #[error("physical address {0} is misaligned for this access")]
    Misaligned(PhysAddr),
    #[error("device access failed: {0}")]
    Device(io::ErrorKind),
}

/// Physical memory as the stock tools see it through `/dev/mem` opened `O_RDWR | O_SYNC`.
///
/// Contract for implementors: accesses reach the device in program order and are not
/// cached (L2 SRAM and MMIO map strongly ordered; DDR at least write-combined and drained
/// before each call returns). Word accesses are single aligned loads/stores. Every
/// address is absolute; the implementor maps the windows it needs (L2 `0x20080000`,
/// core L1, SEC/RCU MMIO under `0x30000000`, the SOC2 DDR buffer).
pub trait PhysicalMemory {
    fn read_u32(&mut self, address: PhysAddr) -> Result<u32, AccessError>;
    fn write_u32(&mut self, address: PhysAddr, value: u32) -> Result<(), AccessError>;
    fn write_u16(&mut self, address: PhysAddr, value: u16) -> Result<(), AccessError>;
    /// Stores `bytes` at ascending addresses; the stock tools use byte-wise `memcpy`.
    fn write_bytes(&mut self, address: PhysAddr, bytes: &[u8]) -> Result<(), AccessError>;
    /// Monotonic time from an arbitrary origin.
    fn now(&self) -> Duration;
    /// Sleeps for about `duration` (the executor passes at most 1 s).
    fn delay(&mut self, duration: Duration);
}

/// `/dev/corectrl` requests. Implementors issue
/// [`crate::intent::core_control_intent`] as one scalar `ioctl`.
pub trait CoreControl {
    fn control(&mut self, core: LoaderCore, action: CoreAction) -> Result<(), AccessError>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    /// SHARC→ARM `0xb0b0` after ARM wrote `0xdead`.
    BooterReady,
    /// `0xbbbb` request for the next chunk.
    Request { block: u32 },
    /// `0x6666` after a chunk was delivered.
    Consumed { block: u32 },
    /// App mailbox `0x5555` from the started kernel.
    AppStart,
    /// App mailbox `0xbbbb` (address ready) or `0xdead` (refused).
    ArgumentBuffer,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum StepError {
    #[error(transparent)]
    Access(#[from] AccessError),
    #[error("timed out in {phase:?} waiting at {address} (last word {last:#x})")]
    Timeout {
        phase: Phase,
        address: PhysAddr,
        last: u32,
    },
    #[error("SHARC requested {requested} bytes but the stream buffer holds {capacity}")]
    Overrun { requested: u32, capacity: u32 },
    #[error("SHARC requested {requested} bytes but only {remaining} remain in the image")]
    Exhausted { requested: u32, remaining: usize },
    #[error("argument buffer address {0:#x} is neither core L1 nor L2")]
    ArgumentRegion(u32),
    #[error("arguments do not fit the window at {0}")]
    ArgumentBounds(PhysAddr),
    #[error("upload source or destination overflows")]
    Overflow,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[error("boot step {step} failed: {error}")]
pub struct BootError {
    pub step: usize,
    #[source]
    pub error: StepError,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamEnd {
    /// SHARC requested 0 bytes.
    ZeroRequest,
    /// SHARC requested `0xffffffff`; ARM answered `0x6666`.
    StopRequest,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StreamReport {
    pub core: LoaderCore,
    pub chunks: u32,
    pub bytes: usize,
    /// Image bytes the SHARC never requested. Stock ignores them.
    pub unconsumed: usize,
    pub end: StreamEnd,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArgumentOutcome {
    Placed {
        address: PhysAddr,
        region: Region,
    },
    /// SHARC answered `0xdead`; ARM wrote `0xbeef` and passed nothing.
    Refused,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ArgumentReport {
    pub core: LoaderCore,
    pub outcome: ArgumentOutcome,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct BootReport {
    pub streams: Vec<StreamReport>,
    pub arguments: Vec<ArgumentReport>,
}

/// Runs every step in order and stops at the first failure.
///
/// Deviations from stock, all on failure paths: stock exits with status 0 after a
/// timeout, ignores a short file read, has no stream-buffer bound and waits forever for
/// the argument buffer; here each is a typed [`BootError`].
pub fn execute<H: PhysicalMemory + CoreControl>(
    plan: &BootPlan<'_>,
    hardware: &mut H,
) -> Result<BootReport, BootError> {
    let mut report = BootReport::default();
    for (index, step) in plan.steps.iter().enumerate() {
        let at = |error: StepError| BootError { step: index, error };
        match *step {
            Step::Core { core, action } => hardware
                .control(core, action)
                .map_err(|error| at(error.into()))?,
            Step::Write32 { address, value } => hardware
                .write_u32(address, value)
                .map_err(|error| at(error.into()))?,
            Step::Write16 { address, value } => hardware
                .write_u16(address, value)
                .map_err(|error| at(error.into()))?,
            Step::Delay(duration) => hardware.delay(duration),
            Step::Upload {
                address,
                swap,
                source,
            } => upload(hardware, address, swap, source).map_err(at)?,
            Step::Stream {
                core,
                mailbox,
                image,
            } => report
                .streams
                .push(stream(hardware, core, &mailbox, image).map_err(at)?),
            Step::Arguments {
                core,
                mailbox,
                arguments,
            } => report.arguments.push(ArgumentReport {
                core,
                outcome: pass_arguments(hardware, core, &mailbox, arguments).map_err(at)?,
            }),
        }
    }
    Ok(report)
}

/// A loadsharc run that failed with a handled error. Stock ignores it and runs the next core.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CoreFailure {
    pub core: LoaderCore,
    pub error: BootError,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Soc1Report {
    pub boot: BootReport,
    pub failures: Vec<CoreFailure>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum Soc1Error {
    #[error("restart quiesce failed: {0}")]
    Quiesce(#[source] BootError),
    #[error("{core:?} load failed on a memory or core-control access: {error}")]
    Access { core: LoaderCore, error: BootError },
}

/// Runs ZenUI's SOC1 sequence. Stock `loadsharc` returns 0 on every handled failure, so
/// core 1 still runs after core 2 fails. Stock exits only when `/dev/mem` cannot be
/// opened or mapped; here a failed access is that fatal case and stops the sequence.
pub fn execute_soc1<H: PhysicalMemory + CoreControl>(
    plan: &Soc1Plan<'_>,
    hardware: &mut H,
) -> Result<Soc1Report, Soc1Error> {
    let quiesce = execute(&plan.quiesce, hardware).map_err(Soc1Error::Quiesce)?;
    let mut report = Soc1Report {
        boot: quiesce,
        failures: Vec::new(),
    };
    for (core, load) in [
        (LoaderCore::Core2, &plan.core2),
        (LoaderCore::Core1, &plan.core1),
    ] {
        match execute(load, hardware) {
            Ok(run) => {
                report.boot.streams.extend(run.streams);
                report.boot.arguments.extend(run.arguments);
            }
            Err(error) if matches!(error.error, StepError::Access(_)) => {
                return Err(Soc1Error::Access { core, error });
            }
            Err(error) => report.failures.push(CoreFailure { core, error }),
        }
    }
    Ok(report)
}

fn upload<M: PhysicalMemory>(
    memory: &mut M,
    address: PhysAddr,
    swap: Swap,
    source: Source<'_>,
) -> Result<(), StepError> {
    address
        .checked_add(source.len())
        .ok_or(StepError::Overflow)?;
    let bytes: Vec<u8> = match source {
        Source::Zeros(len) => vec![0; len as usize],
        Source::Bytes(bytes) => match swap {
            Swap::None => return Ok(memory.write_bytes(address, bytes)?),
            Swap::Words => bytes
                .chunks(4)
                .flat_map(|group| group.iter().rev())
                .copied()
                .collect(),
            Swap::Halfwords => bytes
                .chunks(2)
                .flat_map(|pair| pair.iter().rev())
                .copied()
                .collect(),
        },
    };
    Ok(memory.write_bytes(address, &bytes)?)
}

/// Polls `address` until `accept` returns a value. `interval` zero is the stock busy loop.
fn wait<M: PhysicalMemory, T>(
    memory: &mut M,
    address: PhysAddr,
    phase: Phase,
    interval: Duration,
    accept: impl Fn(u32) -> Option<T>,
) -> Result<T, StepError> {
    let start = memory.now();
    loop {
        let last = memory.read_u32(address)?;
        if let Some(value) = accept(last) {
            return Ok(value);
        }
        if memory.now().saturating_sub(start) > MAILBOX_TIMEOUT {
            return Err(StepError::Timeout {
                phase,
                address,
                last,
            });
        }
        if !interval.is_zero() {
            memory.delay(interval);
        }
    }
}

fn expect(word: u32) -> impl Fn(u32) -> Option<()> {
    move |value| (value == word).then_some(())
}

/// loadsharc `0x8d00..0x9060`.
fn stream<M: PhysicalMemory>(
    memory: &mut M,
    core: LoaderCore,
    mailbox: &StreamMailbox,
    image: &[u8],
) -> Result<StreamReport, StepError> {
    let (out, inbox) = (mailbox.arm_to_sharc, mailbox.sharc_to_arm);
    memory.write_u32(out, word::ARM_READY)?;
    wait(
        memory,
        inbox,
        Phase::BooterReady,
        Duration::ZERO,
        expect(word::BOOTER_READY),
    )?;
    let mut offset = 0;
    let mut chunks = 0;
    let end = loop {
        memory.write_u32(out, word::REQUEST)?;
        let block = chunks;
        wait(
            memory,
            inbox,
            Phase::Request { block },
            Duration::ZERO,
            expect(word::GRANT),
        )?;
        memory.write_u32(out, word::GRANT)?;
        let requested = memory.read_u32(mailbox.to_arm_count)?;
        match requested {
            0 => break StreamEnd::ZeroRequest,
            word::STOP_COUNT => {
                memory.write_u32(out, word::DONE)?;
                break StreamEnd::StopRequest;
            }
            _ => {}
        }
        if requested > mailbox.capacity {
            return Err(StepError::Overrun {
                requested,
                capacity: mailbox.capacity,
            });
        }
        let remaining = image.len() - offset;
        let chunk = image
            .get(offset..offset + requested as usize)
            .ok_or(StepError::Exhausted {
                requested,
                remaining,
            })?;
        memory.write_bytes(mailbox.buffer, chunk)?;
        offset += chunk.len();
        memory.write_u32(mailbox.to_sharc_count, requested)?;
        memory.write_u32(out, word::DONE)?;
        chunks += 1;
        wait(
            memory,
            inbox,
            Phase::Consumed { block },
            Duration::ZERO,
            expect(word::DONE),
        )?;
    };
    Ok(StreamReport {
        core,
        chunks,
        bytes: offset,
        unconsumed: image.len() - offset,
        end,
    })
}

/// loadsharc `0x8fd8..0x9240`. Bounded by [`MAILBOX_TIMEOUT`] where stock waits forever.
fn pass_arguments<M: PhysicalMemory>(
    memory: &mut M,
    core: LoaderCore,
    mailbox: &AppMailbox,
    arguments: &[u8],
) -> Result<ArgumentOutcome, StepError> {
    let (out, inbox) = (mailbox.arm_to_sharc, mailbox.sharc_to_arm);
    wait(
        memory,
        inbox,
        Phase::AppStart,
        APP_START_POLL,
        expect(word::REQUEST),
    )?;
    let len = u32::try_from(arguments.len()).map_err(|_| StepError::Overflow)?;
    memory.write_u32(mailbox.data_to_sharc, len)?;
    memory.write_u32(out, word::REQUEST)?;
    let granted = wait(
        memory,
        inbox,
        Phase::ArgumentBuffer,
        Duration::ZERO,
        |value| matches!(value, word::GRANT | word::ARM_READY).then_some(value == word::GRANT),
    )?;
    if !granted {
        memory.write_u32(out, word::REFUSED)?;
        return Ok(ArgumentOutcome::Refused);
    }
    let local = memory.read_u32(mailbox.data_to_arm)?;
    let map = CoreMap::of(core);
    let (address, region) = map
        .argument_address(local)
        .ok_or(StepError::ArgumentRegion(local))?;
    let fits = address
        .checked_add(len)
        .zip(map.window_end(address))
        .is_some_and(|(end, limit)| end.0 <= limit);
    if !fits {
        return Err(StepError::ArgumentBounds(address));
    }
    memory.write_bytes(address, arguments)?;
    memory.write_u32(out, word::DONE)?;
    Ok(ArgumentOutcome::Placed { address, region })
}

//! UIO notification worker model recovered from ZenUI `0x8a4ec4` (`/dev/uio0`) and `0x8a4d58`
//! (`/dev/uio1`). All I/O is injected.

use std::{
    fmt,
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};

/// `poll(fd, 1, 0xf)` in both workers.
pub const UIO_POLL_INTERVAL: Duration = Duration::from_millis(15);
/// LE u32 `1`, written twice to arm and once after each interrupt read.
pub const UIO_REENABLE: [u8; 4] = 1_u32.to_le_bytes();

/// Cooperative cancellation only. No thread is created by this crate.
#[derive(Debug, Default)]
pub struct Cancellation(AtomicBool);

impl Cancellation {
    pub fn request(&self) {
        self.0.store(true, Ordering::Release);
    }
    pub fn is_requested(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}

/// Protocol ring index passed to the stock ring reader `0x54f374`.
///
/// Wire values match `tetrad_dsp_protocol::CoreId::wire()`. They name host
/// protocol rings, not physical SHARC cores.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProtocolCore {
    Core0 = 0,
    Core1 = 1,
    Core2 = 2,
    Core3 = 3,
    Control = 4,
}

impl ProtocolCore {
    pub const fn wire(self) -> u8 {
        self as u8
    }
}

/// UIO device and the rings its worker drains. There is no default route.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UioRoute {
    /// `/dev/uio0`, worker `0x8a4ec4`.
    Uio0,
    /// `/dev/uio1`, worker `0x8a4d58`.
    Uio1,
}

impl UioRoute {
    /// Drain order on every tick, read directly from the stock workers.
    pub const fn drain_order(self) -> &'static [ProtocolCore] {
        match self {
            Self::Uio0 => &[
                ProtocolCore::Core0,
                ProtocolCore::Control,
                ProtocolCore::Core2,
                ProtocolCore::Core3,
            ],
            Self::Uio1 => &[ProtocolCore::Core1],
        }
    }
}

/// Injected operations for one route. A real adapter must bind the descriptor
/// and rings that match the route; this crate provides none.
pub trait UioOperations {
    type Error;
    /// The backend must enforce an exact four-byte write or return an error.
    fn reenable_interrupts(&mut self, bytes: [u8; 4]) -> Result<(), Self::Error>;
    /// Return true when an interrupt can be read, false on poll timeout.
    fn poll_interrupt(&mut self, timeout: Duration) -> Result<bool, Self::Error>;
    /// The backend must enforce an exact four-byte read or return an error.
    fn read_interrupt_count(&mut self) -> Result<[u8; 4], Self::Error>;
    /// Drain one ring until it is empty. Delivery is backend-owned.
    fn drain_ring(&mut self, core: ProtocolCore) -> Result<(), Self::Error>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkerState {
    /// Constructed with a route; `arm` has not run.
    Unarmed,
    Running,
    Stopped,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkerStage {
    ArmFirst,
    ArmSecond,
    Poll,
    ReadCount,
    Reenable,
    Drain(ProtocolCore),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkerEvent {
    /// Both arming writes completed.
    Armed,
    /// One complete tick: every route ring was drained. `interrupt` holds the
    /// count read on this tick, or `None` on poll timeout.
    Drained { interrupt: Option<u32> },
    /// Cancellation was observed. `after` is the last operation completed in
    /// this call; `None` means this call performed no operation.
    Stopped { after: Option<WorkerStage> },
}

#[derive(Debug, PartialEq, Eq)]
pub enum WorkerError<E> {
    /// A backend operation failed. The worker is now `Failed`.
    Operation { stage: WorkerStage, source: E },
    /// The call is not valid in this state. No operation ran; state is unchanged.
    InvalidState(WorkerState),
}

impl<E: fmt::Display> fmt::Display for WorkerError<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Operation { stage, source } => write!(f, "UIO {stage:?} failed: {source}"),
            Self::InvalidState(state) => write!(f, "UIO worker call invalid in state {state:?}"),
        }
    }
}

impl<E: std::error::Error + 'static> std::error::Error for WorkerError<E> {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Operation { source, .. } => Some(source),
            Self::InvalidState(_) => None,
        }
    }
}

/// Recovered tick order with explicit offline policies:
///
/// - Cancellation is observed before and between operations. An operation in
///   flight completes first. Stock checks its stop flag only at the loop top.
/// - Any operation error latches `Failed`. Nothing is retried or replayed,
///   including a partial arm. Stock ignores `read`/`write` results and treats
///   `poll < 0` as a timeout.
#[derive(Debug)]
pub struct UioWorker {
    route: UioRoute,
    state: WorkerState,
}

impl UioWorker {
    pub const fn new(route: UioRoute) -> Self {
        Self {
            route,
            state: WorkerState::Unarmed,
        }
    }

    pub const fn route(&self) -> UioRoute {
        self.route
    }

    pub const fn state(&self) -> WorkerState {
        self.state
    }

    /// Write `UIO_REENABLE` twice, once per worker lifetime.
    pub fn arm<T: UioOperations>(
        &mut self,
        operations: &mut T,
        cancellation: &Cancellation,
    ) -> Result<WorkerEvent, WorkerError<T::Error>> {
        if self.state != WorkerState::Unarmed {
            return Err(WorkerError::InvalidState(self.state));
        }
        self.finish(arm_sequence(
            operations,
            Tick {
                cancellation,
                after: None,
            },
        ))
    }

    /// One tick: poll; on interrupt read the count and re-enable; then drain
    /// every route ring in order, also after a timeout. `Stopped`/`Failed`
    /// workers return `Stopped { after: None }` without any operation.
    pub fn step<T: UioOperations>(
        &mut self,
        operations: &mut T,
        cancellation: &Cancellation,
    ) -> Result<WorkerEvent, WorkerError<T::Error>> {
        match self.state {
            WorkerState::Running => {}
            WorkerState::Unarmed => return Err(WorkerError::InvalidState(self.state)),
            WorkerState::Stopped | WorkerState::Failed => {
                return Ok(WorkerEvent::Stopped { after: None });
            }
        }
        self.finish(tick_sequence(
            self.route,
            operations,
            Tick {
                cancellation,
                after: None,
            },
        ))
    }

    fn finish<E>(
        &mut self,
        result: Result<WorkerEvent, Exit<E>>,
    ) -> Result<WorkerEvent, WorkerError<E>> {
        match result {
            Ok(event) => {
                self.state = WorkerState::Running;
                Ok(event)
            }
            Err(Exit::Stopped { after }) => {
                self.state = WorkerState::Stopped;
                Ok(WorkerEvent::Stopped { after })
            }
            Err(Exit::Failed { stage, source }) => {
                self.state = WorkerState::Failed;
                Err(WorkerError::Operation { stage, source })
            }
        }
    }
}

fn arm_sequence<T: UioOperations>(
    operations: &mut T,
    mut tick: Tick<'_>,
) -> Result<WorkerEvent, Exit<T::Error>> {
    for stage in [WorkerStage::ArmFirst, WorkerStage::ArmSecond] {
        tick.run(stage, || operations.reenable_interrupts(UIO_REENABLE))?;
    }
    Ok(WorkerEvent::Armed)
}

fn tick_sequence<T: UioOperations>(
    route: UioRoute,
    operations: &mut T,
    mut tick: Tick<'_>,
) -> Result<WorkerEvent, Exit<T::Error>> {
    let ready = tick.run(WorkerStage::Poll, || {
        operations.poll_interrupt(UIO_POLL_INTERVAL)
    })?;
    let interrupt = if ready {
        let count = tick.run(WorkerStage::ReadCount, || operations.read_interrupt_count())?;
        tick.run(WorkerStage::Reenable, || {
            operations.reenable_interrupts(UIO_REENABLE)
        })?;
        Some(u32::from_le_bytes(count))
    } else {
        None
    };
    for &core in route.drain_order() {
        tick.run(WorkerStage::Drain(core), || operations.drain_ring(core))?;
    }
    Ok(WorkerEvent::Drained { interrupt })
}

enum Exit<E> {
    Stopped { after: Option<WorkerStage> },
    Failed { stage: WorkerStage, source: E },
}

/// Per-call sequencer: checks cancellation before each operation and records
/// the last completed stage.
struct Tick<'a> {
    cancellation: &'a Cancellation,
    after: Option<WorkerStage>,
}

impl Tick<'_> {
    fn run<T, E>(
        &mut self,
        stage: WorkerStage,
        operation: impl FnOnce() -> Result<T, E>,
    ) -> Result<T, Exit<E>> {
        if self.cancellation.is_requested() {
            return Err(Exit::Stopped { after: self.after });
        }
        let value = operation().map_err(|source| Exit::Failed { stage, source })?;
        self.after = Some(stage);
        Ok(value)
    }
}

/// An integration backend owns any real worker and join handle.
pub trait WorkerShutdown {
    type Error;
    fn request_cancel(&mut self);
    fn join(&mut self) -> Result<(), Self::Error>;
}

pub fn shutdown<W: WorkerShutdown>(worker: &mut W) -> Result<(), W::Error> {
    worker.request_cancel();
    worker.join()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ProtocolCore::{Control, Core0, Core1, Core2, Core3};

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Op {
        Write([u8; 4]),
        Poll(Duration),
        Read,
        Drain(ProtocolCore),
    }

    const FAILURE: &str = "synthetic failure";
    const COUNT: [u8; 4] = [0x78, 0x56, 0x34, 0x12];

    /// Records every call; fails or requests cancellation inside call `n`.
    struct Fake<'a> {
        calls: Vec<Op>,
        ready: bool,
        fail_at: Option<usize>,
        cancel_at: Option<usize>,
        cancellation: &'a Cancellation,
    }

    impl<'a> Fake<'a> {
        fn new(cancellation: &'a Cancellation) -> Self {
            Self {
                calls: vec![],
                ready: true,
                fail_at: None,
                cancel_at: None,
                cancellation,
            }
        }
        fn call(&mut self, op: Op) -> Result<(), &'static str> {
            let n = self.calls.len();
            self.calls.push(op);
            if self.cancel_at == Some(n) {
                self.cancellation.request();
            }
            if self.fail_at == Some(n) {
                return Err(FAILURE);
            }
            Ok(())
        }
    }

    impl UioOperations for Fake<'_> {
        type Error = &'static str;
        fn reenable_interrupts(&mut self, bytes: [u8; 4]) -> Result<(), Self::Error> {
            self.call(Op::Write(bytes))
        }
        fn poll_interrupt(&mut self, timeout: Duration) -> Result<bool, Self::Error> {
            self.call(Op::Poll(timeout)).map(|()| self.ready)
        }
        fn read_interrupt_count(&mut self) -> Result<[u8; 4], Self::Error> {
            self.call(Op::Read).map(|()| COUNT)
        }
        fn drain_ring(&mut self, core: ProtocolCore) -> Result<(), Self::Error> {
            self.call(Op::Drain(core))
        }
    }

    const ARM: Op = Op::Write([1, 0, 0, 0]);
    const POLL: Op = Op::Poll(Duration::from_millis(15));

    /// Arm, one interrupt tick on uio0, and the stage each call belongs to.
    const UIO0_IRQ_SEQUENCE: [(Op, WorkerStage); 9] = [
        (ARM, WorkerStage::ArmFirst),
        (ARM, WorkerStage::ArmSecond),
        (POLL, WorkerStage::Poll),
        (Op::Read, WorkerStage::ReadCount),
        (ARM, WorkerStage::Reenable),
        (Op::Drain(Core0), WorkerStage::Drain(Core0)),
        (Op::Drain(Control), WorkerStage::Drain(Control)),
        (Op::Drain(Core2), WorkerStage::Drain(Core2)),
        (Op::Drain(Core3), WorkerStage::Drain(Core3)),
    ];

    /// Arm then step until the worker leaves `Running` or two ticks complete.
    fn drive(
        worker: &mut UioWorker,
        fake: &mut Fake<'_>,
    ) -> Result<WorkerEvent, WorkerError<&'static str>> {
        let cancellation = fake.cancellation;
        let mut result = worker.arm(fake, cancellation);
        for _ in 0..2 {
            if worker.state() != WorkerState::Running {
                break;
            }
            result = worker.step(fake, cancellation);
        }
        result
    }

    fn assert_halted(worker: &mut UioWorker, fake: &mut Fake<'_>, state: WorkerState) {
        let calls = fake.calls.len();
        let cancellation = fake.cancellation;
        assert_eq!(worker.state(), state);
        assert_eq!(
            worker.arm(fake, cancellation),
            Err(WorkerError::InvalidState(state))
        );
        assert_eq!(
            worker.step(fake, cancellation),
            Ok(WorkerEvent::Stopped { after: None })
        );
        assert_eq!(fake.calls.len(), calls, "no work after {state:?}");
    }

    #[test]
    fn routes_arm_then_drain_every_tick_in_recovered_order() {
        assert_eq!(UioRoute::Uio0.drain_order(), [Core0, Control, Core2, Core3]);
        assert_eq!(UioRoute::Uio1.drain_order(), [Core1]);
        assert_eq!(
            [Core0, Core1, Core2, Core3, Control].map(ProtocolCore::wire),
            [0, 1, 2, 3, 4]
        );

        for route in [UioRoute::Uio0, UioRoute::Uio1] {
            let cancellation = Cancellation::default();
            let mut fake = Fake::new(&cancellation);
            let mut worker = UioWorker::new(route);
            let drains = route.drain_order().iter().map(|&core| Op::Drain(core));

            assert_eq!(worker.arm(&mut fake, &cancellation), Ok(WorkerEvent::Armed));
            assert_eq!(fake.calls, [ARM, ARM]);

            fake.calls.clear();
            fake.ready = false;
            assert_eq!(
                worker.step(&mut fake, &cancellation),
                Ok(WorkerEvent::Drained { interrupt: None })
            );
            assert!(
                fake.calls
                    .iter()
                    .copied()
                    .eq([POLL].into_iter().chain(drains.clone()))
            );

            fake.calls.clear();
            fake.ready = true;
            assert_eq!(
                worker.step(&mut fake, &cancellation),
                Ok(WorkerEvent::Drained {
                    interrupt: Some(0x1234_5678)
                })
            );
            let irq = [POLL, Op::Read, ARM].into_iter().chain(drains);
            assert!(fake.calls.iter().copied().eq(irq));
            assert_eq!(worker.state(), WorkerState::Running);
        }
    }

    #[test]
    fn arm_is_one_time_and_step_requires_arm() {
        let cancellation = Cancellation::default();
        let mut fake = Fake::new(&cancellation);
        let mut worker = UioWorker::new(UioRoute::Uio1);
        assert_eq!(
            worker.step(&mut fake, &cancellation),
            Err(WorkerError::InvalidState(WorkerState::Unarmed))
        );
        assert_eq!(worker.state(), WorkerState::Unarmed);
        assert!(fake.calls.is_empty());

        assert_eq!(worker.arm(&mut fake, &cancellation), Ok(WorkerEvent::Armed));
        assert_eq!(
            worker.arm(&mut fake, &cancellation),
            Err(WorkerError::InvalidState(WorkerState::Running))
        );
        assert_eq!(fake.calls, [ARM, ARM]);
        assert_eq!(worker.state(), WorkerState::Running);
    }

    #[test]
    fn cancellation_between_operations_stops_further_work() {
        let cancellation = Cancellation::default();
        cancellation.request();
        let mut fake = Fake::new(&cancellation);
        let mut worker = UioWorker::new(UioRoute::Uio0);
        assert_eq!(
            worker.arm(&mut fake, &cancellation),
            Ok(WorkerEvent::Stopped { after: None })
        );
        assert_halted(&mut worker, &mut fake, WorkerState::Stopped);
        assert!(fake.calls.is_empty());

        let last = UIO0_IRQ_SEQUENCE.len() - 1;
        for (i, (_, stage)) in UIO0_IRQ_SEQUENCE.into_iter().enumerate() {
            let cancellation = Cancellation::default();
            let mut fake = Fake::new(&cancellation);
            fake.cancel_at = Some(i);
            let mut worker = UioWorker::new(UioRoute::Uio0);
            // A request during the final operation of a call is seen by the next call.
            let after = (i != 1 && i != last).then_some(stage);
            assert_eq!(
                drive(&mut worker, &mut fake),
                Ok(WorkerEvent::Stopped { after })
            );
            assert!(
                fake.calls
                    .iter()
                    .eq(UIO0_IRQ_SEQUENCE[..=i].iter().map(|(op, _)| op))
            );
            assert_halted(&mut worker, &mut fake, WorkerState::Stopped);
        }
    }

    #[test]
    fn failures_latch_at_failed_operation_without_retry() {
        for (i, (_, stage)) in UIO0_IRQ_SEQUENCE.into_iter().enumerate() {
            let cancellation = Cancellation::default();
            let mut fake = Fake::new(&cancellation);
            fake.fail_at = Some(i);
            let mut worker = UioWorker::new(UioRoute::Uio0);
            assert_eq!(
                drive(&mut worker, &mut fake),
                Err(WorkerError::Operation {
                    stage,
                    source: FAILURE
                })
            );
            assert!(
                fake.calls
                    .iter()
                    .eq(UIO0_IRQ_SEQUENCE[..=i].iter().map(|(op, _)| op))
            );
            // Includes a partial arm (i == 1): the first write is not redone.
            assert_halted(&mut worker, &mut fake, WorkerState::Failed);
        }
    }

    #[test]
    fn shutdown_requests_cancel_before_join_and_preserves_error() {
        struct FakeWorker(Vec<&'static str>);
        impl WorkerShutdown for FakeWorker {
            type Error = &'static str;
            fn request_cancel(&mut self) {
                self.0.push("cancel");
            }
            fn join(&mut self) -> Result<(), Self::Error> {
                self.0.push("join");
                Err("synthetic join failure")
            }
        }
        let mut w = FakeWorker(vec![]);
        assert_eq!(shutdown(&mut w), Err("synthetic join failure"));
        assert_eq!(w.0, ["cancel", "join"]);
    }
}

use std::{num::NonZeroUsize, time::Duration};

use tetrad_runtime::watchdog::{
    Clock, ClockError, ProcessPresence, Watchdog, WatchdogError, WatchdogEvent, WatchdogState,
};
use thiserror::Error;

use crate::process::{ProcessSource, ProcessTarget, SnapshotError, presence};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeliveryId(u64);

impl DeliveryId {
    pub fn get(self) -> u64 {
        self.0
    }
}

/// Inert outputs. There is deliberately no executor for reboot or process control.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Effect {
    None,
    ShowCrash,
    SetCountdown { seconds: u8 },
    RebootIntent,
    DismissCrash,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Delivery {
    pub id: DeliveryId,
    pub event: WatchdogEvent,
    pub effect: Effect,
}

#[derive(Debug, PartialEq, Eq, Error)]
pub enum SupervisorError<E> {
    #[error("{0}")]
    Clock(ClockError),
    #[error("process source failed: {0}")]
    Source(E),
    #[error("{0}")]
    Snapshot(SnapshotError),
    #[error("delivery ID exhausted")]
    DeliveryOverflow,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum AcknowledgeError {
    #[error("no delivery is pending")]
    NoPending,
    #[error("delivery {received:?} does not match pending {expected:?}")]
    WrongId {
        expected: DeliveryId,
        received: DeliveryId,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum CancelError {
    #[error("acknowledge the pending delivery before cancellation")]
    PendingDelivery,
    #[error("delivery ID exhausted")]
    DeliveryOverflow,
}

/// A single-process monitor with bounded, acknowledged event delivery.
///
/// Only one transition can be pending. Until acknowledged, polls return that
/// same delivery without reading either injected interface or advancing time.
/// Acknowledgement means the caller accepted the intent, not that an effect ran.
/// Keep the delivery ID when retrying a consumer: retries are not exactly-once
/// execution. IDs are unique only within one supervisor instance.
#[derive(Debug)]
pub struct Supervisor {
    watchdog: Watchdog,
    target: ProcessTarget,
    maximum_processes: NonZeroUsize,
    last_delivery: u64,
    pending: Option<Delivery>,
}

impl Supervisor {
    pub fn new(
        start: Duration,
        target: ProcessTarget,
        maximum_processes: NonZeroUsize,
    ) -> Result<Self, ClockError> {
        Ok(Self {
            watchdog: Watchdog::new(start)?,
            target,
            maximum_processes,
            last_delivery: 0,
            pending: None,
        })
    }

    pub fn state(&self) -> WatchdogState {
        self.watchdog.state()
    }

    pub fn target(&self) -> &ProcessTarget {
        &self.target
    }

    /// No runnable deadline while awaiting acknowledgement or after termination.
    pub fn next_deadline(&self) -> Option<Duration> {
        if self.pending.is_some() {
            None
        } else {
            self.watchdog.next_deadline()
        }
    }

    pub fn pending(&self) -> Option<&Delivery> {
        self.pending.as_ref()
    }

    /// Service at most one recovered timer callback. Errors retain the due
    /// callback and counters; accepted clock readings retain their watermark.
    /// Late polls never fabricate earlier observations or countdown ticks.
    pub fn poll<C: Clock, P: ProcessSource>(
        &mut self,
        clock: &C,
        processes: &mut P,
    ) -> Result<Option<&Delivery>, SupervisorError<P::Error>> {
        if self.pending.is_some() || self.watchdog.next_deadline().is_none() {
            return Ok(self.pending.as_ref());
        }
        let id = self
            .last_delivery
            .checked_add(1)
            .ok_or(SupervisorError::DeliveryOverflow)?;
        let mut observation = Observation {
            processes,
            target: &self.target,
            maximum: self.maximum_processes.get(),
        };
        let event = self
            .watchdog
            .poll(clock, &mut observation)
            .map_err(|error| match error {
                WatchdogError::Clock(error) => SupervisorError::Clock(error),
                WatchdogError::Presence(ObservationError::Source(error)) => {
                    SupervisorError::Source(error)
                }
                WatchdogError::Presence(ObservationError::Snapshot(error)) => {
                    SupervisorError::Snapshot(error)
                }
            })?;
        if let Some(event) = event {
            self.stage(id, event);
        }
        Ok(self.pending.as_ref())
    }

    pub fn acknowledge(&mut self, id: DeliveryId) -> Result<(), AcknowledgeError> {
        let delivery = self.pending.as_ref().ok_or(AcknowledgeError::NoPending)?;
        if delivery.id != id {
            return Err(AcknowledgeError::WrongId {
                expected: delivery.id,
                received: id,
            });
        }
        self.pending = None;
        Ok(())
    }

    /// Owned explicit shutdown policy. A pending delivery cannot be silently
    /// discarded. Cancellation can stop an armed countdown but never retracts
    /// a reboot intent already issued. Stock start/stop slots are not modeled.
    pub fn cancel(&mut self) -> Result<Option<&Delivery>, CancelError> {
        if self.pending.is_some() {
            return Err(CancelError::PendingDelivery);
        }
        if self.watchdog.next_deadline().is_none() {
            return Ok(None);
        }
        let id = self
            .last_delivery
            .checked_add(1)
            .ok_or(CancelError::DeliveryOverflow)?;
        if let Some(event) = self.watchdog.cancel() {
            self.stage(id, event);
        }
        Ok(self.pending.as_ref())
    }

    fn stage(&mut self, id: u64, event: WatchdogEvent) {
        let effect = match event {
            WatchdogEvent::PresenceChecked { .. } => Effect::None,
            WatchdogEvent::CountdownStarted { .. } => Effect::ShowCrash,
            WatchdogEvent::CountdownTick { seconds } => Effect::SetCountdown { seconds },
            WatchdogEvent::RebootRequested => Effect::RebootIntent,
            WatchdogEvent::Cancelled => Effect::DismissCrash,
        };
        self.last_delivery = id;
        self.pending = Some(Delivery {
            id: DeliveryId(id),
            event,
            effect,
        });
    }
}

struct Observation<'a, P> {
    processes: &'a mut P,
    target: &'a ProcessTarget,
    maximum: usize,
}

enum ObservationError<E> {
    Source(E),
    Snapshot(SnapshotError),
}

impl<P: ProcessSource> ProcessPresence for Observation<'_, P> {
    type Error = ObservationError<P::Error>;

    fn is_present(&mut self) -> Result<bool, Self::Error> {
        let records = self
            .processes
            .snapshot()
            .map_err(ObservationError::Source)?;
        presence(records, self.target, self.maximum).map_err(ObservationError::Snapshot)
    }
}

#[cfg(test)]
mod tests {
    use std::{cell::Cell, num::NonZeroU32};

    use crate::process::{ProcessIdentity, ProcessName, ProcessRecord, fixture};

    use super::*;

    struct Time {
        now: Duration,
        calls: Cell<usize>,
    }

    impl Clock for Time {
        fn now(&self) -> Duration {
            self.calls.set(self.calls.get() + 1);
            self.now
        }
    }

    struct Processes {
        records: Vec<ProcessRecord>,
        fail: bool,
        calls: usize,
    }

    impl ProcessSource for Processes {
        type Error = &'static str;

        fn snapshot(&mut self) -> Result<&[ProcessRecord], Self::Error> {
            self.calls += 1;
            if self.fail {
                Err("synthetic failure")
            } else {
                Ok(&self.records)
            }
        }
    }

    fn time(ms: u64) -> Time {
        Time {
            now: Duration::from_millis(ms),
            calls: Cell::new(0),
        }
    }

    fn setup() -> (Supervisor, Processes) {
        (
            Supervisor::new(
                Duration::ZERO,
                ProcessTarget::Name(ProcessName::new(b"app").unwrap()),
                NonZeroUsize::new(2).unwrap(),
            )
            .unwrap(),
            Processes {
                records: Vec::new(),
                fail: false,
                calls: 0,
            },
        )
    }

    fn deliver(supervisor: &mut Supervisor, processes: &mut Processes, ms: u64) -> Delivery {
        let delivery = *supervisor.poll(&time(ms), processes).unwrap().unwrap();
        supervisor.acknowledge(delivery.id).unwrap();
        delivery
    }

    fn record(pid: u32, name: &[u8], state: u8, start_ticks: u64) -> ProcessRecord {
        ProcessRecord::parse_stat(&fixture(pid, name, state, start_ticks)).unwrap()
    }

    #[test]
    fn countdown_effects_follow_recovered_callbacks_without_process_control() {
        let (mut supervisor, mut processes) = setup();
        processes.records.push(record(42, b"unrelated", b'R', 1));
        let original = processes.records.clone();
        assert_eq!(supervisor.poll(&time(499), &mut processes).unwrap(), None);
        assert_eq!(processes.calls, 0);
        for (index, ms) in (500..=2_500).step_by(500).enumerate() {
            let delivery = deliver(&mut supervisor, &mut processes, ms);
            assert_eq!(delivery.id.get(), index as u64 + 1);
            assert_eq!(delivery.effect, Effect::None);
            assert_eq!(
                delivery.event,
                WatchdogEvent::PresenceChecked {
                    present: false,
                    consecutive_missed_ms: ms as u16,
                }
            );
        }
        let armed = deliver(&mut supervisor, &mut processes, 3_000);
        assert_eq!(armed.event, WatchdogEvent::CountdownStarted { seconds: 5 });
        assert_eq!(armed.effect, Effect::ShowCrash);
        assert_eq!(supervisor.poll(&time(3_999), &mut processes).unwrap(), None);
        // Even a returned target does not cancel stock countdown behavior.
        processes.records.push(record(1, b"app", b'R', 1));
        for (ms, seconds) in [(4_000, 5), (5_000, 4), (6_000, 3), (7_000, 2), (8_000, 1)] {
            assert_eq!(
                deliver(&mut supervisor, &mut processes, ms).effect,
                Effect::SetCountdown { seconds }
            );
        }
        assert_eq!(supervisor.poll(&time(8_999), &mut processes).unwrap(), None);
        let reboot = *supervisor
            .poll(&time(9_000), &mut processes)
            .unwrap()
            .unwrap();
        assert_eq!(reboot.event, WatchdogEvent::RebootRequested);
        assert_eq!(reboot.effect, Effect::RebootIntent);
        assert_eq!(supervisor.next_deadline(), None);
        assert_eq!(
            supervisor.poll(&time(99_000), &mut processes).unwrap(),
            Some(&reboot)
        );
        supervisor.acknowledge(reboot.id).unwrap();
        assert_eq!(
            supervisor.poll(&time(99_000), &mut processes).unwrap(),
            None
        );
        assert_eq!(supervisor.cancel().unwrap(), None);
        assert_eq!(processes.calls, 6);
        assert_eq!(processes.records[0], original[0]);
    }

    #[test]
    fn pending_delivery_applies_backpressure_and_rejects_stale_acknowledgement() {
        let (mut supervisor, mut processes) = setup();
        let first = deliver(&mut supervisor, &mut processes, 500);
        let second = *supervisor
            .poll(&time(1_000), &mut processes)
            .unwrap()
            .unwrap();
        let late = time(50_000);
        assert_eq!(
            supervisor.poll(&late, &mut processes).unwrap(),
            Some(&second)
        );
        assert_eq!(late.calls.get(), 0);
        assert_eq!(processes.calls, 2);
        assert_eq!(supervisor.next_deadline(), None);
        assert_eq!(supervisor.cancel(), Err(CancelError::PendingDelivery));
        assert_eq!(
            supervisor.acknowledge(first.id),
            Err(AcknowledgeError::WrongId {
                expected: second.id,
                received: first.id,
            })
        );
        assert_eq!(supervisor.pending(), Some(&second));
        supervisor.acknowledge(second.id).unwrap();
        assert_eq!(
            supervisor.acknowledge(second.id),
            Err(AcknowledgeError::NoPending)
        );
        assert_eq!(
            supervisor.next_deadline(),
            Some(Duration::from_millis(1_500))
        );
        let third = deliver(&mut supervisor, &mut processes, 50_000);
        assert_eq!(
            third.event,
            WatchdogEvent::PresenceChecked {
                present: false,
                consecutive_missed_ms: 1_500
            }
        );
        assert_eq!(
            supervisor.next_deadline(),
            Some(Duration::from_millis(50_500))
        );
    }

    #[test]
    fn source_and_snapshot_errors_preserve_counter_deadline_and_clock_watermark() {
        let (mut supervisor, mut processes) = setup();
        processes.fail = true;
        assert_eq!(
            supervisor.poll(&time(10_000), &mut processes),
            Err(SupervisorError::Source("synthetic failure"))
        );
        assert_eq!(
            supervisor.state(),
            WatchdogState::Monitoring {
                consecutive_missed_ms: 0
            }
        );
        assert_eq!(supervisor.pending(), None);
        assert_eq!(supervisor.next_deadline(), Some(Duration::from_millis(500)));
        processes.fail = false;
        assert!(matches!(
            supervisor.poll(&time(9_999), &mut processes),
            Err(SupervisorError::Clock(ClockError::Rollback { .. }))
        ));
        assert_eq!(processes.calls, 1);
        processes.records = vec![record(1, b"app", b'R', 1), record(1, b"other", b'R', 2)];
        assert_eq!(
            supervisor.poll(&time(10_000), &mut processes),
            Err(SupervisorError::Snapshot(SnapshotError::DuplicatePid(
                NonZeroU32::new(1).unwrap()
            )))
        );
        processes.records.push(record(2, b"other", b'R', 2));
        assert_eq!(
            supervisor.poll(&time(10_001), &mut processes),
            Err(SupervisorError::Snapshot(SnapshotError::TooMany {
                actual: 3,
                maximum: 2
            }))
        );
        assert!(matches!(
            supervisor.poll(&time(10_000), &mut processes),
            Err(SupervisorError::Clock(ClockError::Rollback { .. }))
        ));
        assert_eq!(supervisor.next_deadline(), Some(Duration::from_millis(500)));
        processes.records.clear();
        assert_eq!(deliver(&mut supervisor, &mut processes, 10_001).id.get(), 1);
    }

    #[test]
    fn exact_presence_resets_misses_but_reused_identity_does_not() {
        let (mut supervisor, mut processes) = setup();
        for ms in (500..=2_500).step_by(500) {
            deliver(&mut supervisor, &mut processes, ms);
        }
        // A modern-kernel worker comm longer than 15 bytes must not turn the snapshot into an error.
        let worker = record(2, b"kworker/u16:2-events_unbound", b'I', 3);
        let app = record(1, b"app", b'R', 10);
        processes.records = vec![worker.clone(), app.clone()];
        assert_eq!(
            deliver(&mut supervisor, &mut processes, 3_000).event,
            WatchdogEvent::PresenceChecked {
                present: true,
                consecutive_missed_ms: 0
            }
        );
        let mut pinned = Supervisor::new(
            Duration::ZERO,
            ProcessTarget::Identity(ProcessIdentity {
                start_ticks: 9,
                ..app.identity()
            }),
            NonZeroUsize::new(2).unwrap(),
        )
        .unwrap();
        assert_eq!(
            deliver(&mut pinned, &mut processes, 500).event,
            WatchdogEvent::PresenceChecked {
                present: false,
                consecutive_missed_ms: 500
            }
        );
        assert_eq!(processes.records, vec![worker, app]);
    }

    #[test]
    fn cancellation_stops_monitoring_and_countdown_without_retracting_outputs() {
        for armed in [false, true] {
            let (mut supervisor, mut processes) = setup();
            if armed {
                for ms in (500..=3_000).step_by(500) {
                    deliver(&mut supervisor, &mut processes, ms);
                }
            }
            let calls = processes.calls;
            let cancelled = *supervisor.cancel().unwrap().unwrap();
            assert_eq!(cancelled.event, WatchdogEvent::Cancelled);
            assert_eq!(cancelled.effect, Effect::DismissCrash);
            assert_eq!(supervisor.cancel(), Err(CancelError::PendingDelivery));
            supervisor.acknowledge(cancelled.id).unwrap();
            let now = time(99_000);
            assert_eq!(supervisor.poll(&now, &mut processes).unwrap(), None);
            assert_eq!(supervisor.cancel().unwrap(), None);
            assert_eq!(supervisor.state(), WatchdogState::Cancelled);
            assert_eq!(processes.calls, calls);
            assert_eq!(now.calls.get(), 0);
        }
    }

    #[test]
    fn clock_and_delivery_overflow_refuse_before_process_observation() {
        let (mut supervisor, mut processes) = setup();
        supervisor.last_delivery = u64::MAX;
        let now = time(500);
        assert_eq!(
            supervisor.poll(&now, &mut processes),
            Err(SupervisorError::DeliveryOverflow)
        );
        assert_eq!(supervisor.cancel(), Err(CancelError::DeliveryOverflow));
        assert_eq!(processes.calls, 0);
        assert_eq!(now.calls.get(), 0);
        assert_eq!(
            supervisor.state(),
            WatchdogState::Monitoring {
                consecutive_missed_ms: 0
            }
        );
        assert!(matches!(
            Supervisor::new(
                Duration::MAX,
                supervisor.target.clone(),
                NonZeroUsize::new(1).unwrap()
            ),
            Err(ClockError::Overflow)
        ));
        let mut near_end = Supervisor::new(
            Duration::MAX - Duration::from_secs(2),
            supervisor.target.clone(),
            NonZeroUsize::new(1).unwrap(),
        )
        .unwrap();
        let before = near_end.next_deadline();
        assert_eq!(
            near_end.poll(
                &Time {
                    now: Duration::MAX,
                    calls: Cell::new(0)
                },
                &mut processes
            ),
            Err(SupervisorError::Clock(ClockError::Overflow))
        );
        assert_eq!(processes.calls, 0);
        assert_eq!(near_end.next_deadline(), before);
    }
}

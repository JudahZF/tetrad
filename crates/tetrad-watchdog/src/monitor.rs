//! Bounded monitoring runtime: a monotonic clock, a sleeper and a finite loop that
//! polls a [`Supervisor`], acknowledges each delivery and hands it to the caller.
//! Deliveries are reported, never executed. The loop ends on its sample budget,
//! its duration budget, or the supervisor's terminal state.

use std::{
    num::NonZeroU32,
    time::{Duration, Instant},
};

use tetrad_runtime::watchdog::{Clock, ClockError, PRESENCE_INTERVAL};
use thiserror::Error;

use crate::{
    process::ProcessSource,
    supervisor::{Delivery, Supervisor, SupervisorError},
};

/// `std::time::Instant` elapsed since construction. Never moves backward on Linux.
#[derive(Debug, Clone, Copy)]
pub struct MonotonicClock {
    origin: Instant,
}

impl MonotonicClock {
    pub fn new() -> Self {
        Self {
            origin: Instant::now(),
        }
    }
}

impl Default for MonotonicClock {
    fn default() -> Self {
        Self::new()
    }
}

impl Clock for MonotonicClock {
    fn now(&self) -> Duration {
        self.origin.elapsed()
    }
}

pub trait Sleep {
    fn sleep(&mut self, duration: Duration);
}

/// Blocks the calling thread.
#[derive(Debug, Default, Clone, Copy)]
pub struct ThreadSleep;

impl Sleep for ThreadSleep {
    fn sleep(&mut self, duration: Duration) {
        std::thread::sleep(duration);
    }
}

/// Owned CLI limits so a smoke check cannot run unbounded.
pub const MAX_SAMPLES: u32 = 10_000;
pub const MAX_DURATION: Duration = Duration::from_secs(3_600);

/// At least one limit is required; the first reached ends the run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Budget {
    samples: Option<NonZeroU32>,
    duration: Option<Duration>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum BudgetError {
    #[error("a sample or duration limit is required")]
    Unbounded,
    #[error("sample limit exceeds {MAX_SAMPLES}")]
    TooManySamples,
    #[error("duration limit exceeds {MAX_DURATION:?}")]
    TooLong,
}

impl Budget {
    pub fn new(
        samples: Option<NonZeroU32>,
        duration: Option<Duration>,
    ) -> Result<Self, BudgetError> {
        if samples.is_none() && duration.is_none() {
            return Err(BudgetError::Unbounded);
        }
        if samples.is_some_and(|limit| limit.get() > MAX_SAMPLES) {
            return Err(BudgetError::TooManySamples);
        }
        if duration.is_some_and(|limit| limit > MAX_DURATION || limit.is_zero()) {
            return Err(BudgetError::TooLong);
        }
        Ok(Self { samples, duration })
    }

    pub fn samples(&self) -> Option<NonZeroU32> {
        self.samples
    }

    pub fn duration(&self) -> Option<Duration> {
        self.duration
    }
}

/// One loop iteration result handed to the caller. Errors count as samples so a
/// persistently failing source still exhausts the budget.
#[derive(Debug)]
pub enum Sample<E> {
    Delivered {
        at: Duration,
        delivery: Delivery,
    },
    Failed {
        at: Duration,
        error: SupervisorError<E>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stop {
    SampleBudget,
    DurationBudget,
    /// The supervisor has no further deadline: a reboot intent was staged and
    /// acknowledged, or monitoring was cancelled. Nothing was executed.
    Terminal,
}

/// Run until the budget or terminal state. Clock faults abort because the recovered
/// timer model cannot continue after rollback or overflow; source and snapshot
/// errors are reported, counted and retried after one presence interval.
///
/// The duration budget is **cooperative**: the clock is read before and after the
/// synchronous source/sink work, and the loop stops before sleeping once the
/// budget is spent, but it cannot interrupt a slow snapshot or sink in progress.
/// A clock that moves backward between those reads is a `Rollback` error.
pub fn run<C: Clock, P: ProcessSource, S: Sleep>(
    supervisor: &mut Supervisor,
    clock: &C,
    source: &mut P,
    budget: Budget,
    sleeper: &mut S,
    mut sink: impl FnMut(Sample<P::Error>),
) -> Result<Stop, ClockError> {
    let start = clock.now();
    let mut watermark = start;
    let mut samples = 0_u32;
    loop {
        let now = read(clock, &mut watermark)?;
        if remaining(budget, start, now).is_some_and(|left| left.is_zero()) {
            return Ok(Stop::DurationBudget);
        }
        let retry = match supervisor.poll(clock, source) {
            Ok(Some(delivery)) => {
                let delivery = *delivery;
                supervisor
                    .acknowledge(delivery.id)
                    .expect("delivery returned by this poll is pending");
                sink(Sample::Delivered { at: now, delivery });
                samples += 1;
                None
            }
            Ok(None) => None,
            Err(SupervisorError::Clock(error)) => return Err(error),
            Err(error) => {
                sink(Sample::Failed { at: now, error });
                samples += 1;
                Some(PRESENCE_INTERVAL)
            }
        };
        // Source and sink work took real time; budget the sleep from a fresh reading.
        let after = read(clock, &mut watermark)?;
        if budget.samples.is_some_and(|limit| samples >= limit.get()) {
            return Ok(Stop::SampleBudget);
        }
        let left = remaining(budget, start, after);
        if left.is_some_and(|left| left.is_zero()) {
            return Ok(Stop::DurationBudget);
        }
        let wait = match retry {
            Some(interval) => interval,
            None => {
                let Some(deadline) = supervisor.next_deadline() else {
                    return Ok(Stop::Terminal);
                };
                deadline.saturating_sub(after)
            }
        };
        let wait = left.map_or(wait, |left| wait.min(left));
        if !wait.is_zero() {
            sleeper.sleep(wait);
        }
    }
}

/// Monotonic reading relative to this run's own watermark. The supervisor keeps
/// its own watermark; this one also covers time spent in the source and sink.
fn read<C: Clock>(clock: &C, watermark: &mut Duration) -> Result<Duration, ClockError> {
    let now = clock.now();
    if now < *watermark {
        return Err(ClockError::Rollback {
            previous: *watermark,
            now,
        });
    }
    *watermark = now;
    Ok(now)
}

fn remaining(budget: Budget, start: Duration, now: Duration) -> Option<Duration> {
    budget
        .duration
        .map(|limit| limit.saturating_sub(now.saturating_sub(start)))
}

#[cfg(test)]
mod tests {
    use std::{cell::Cell, num::NonZeroUsize, rc::Rc};

    use tetrad_runtime::watchdog::WatchdogEvent;

    use super::*;
    use crate::{
        process::{ProcessName, ProcessRecord, ProcessTarget, fixture},
        supervisor::Effect,
    };

    #[derive(Clone)]
    struct Virtual {
        now: Rc<Cell<Duration>>,
        sleeps: Rc<Cell<Vec<Duration>>>,
    }

    impl Virtual {
        fn new() -> Self {
            Self {
                now: Rc::new(Cell::new(Duration::ZERO)),
                sleeps: Rc::new(Cell::new(Vec::new())),
            }
        }

        fn sleeps(&self) -> Vec<Duration> {
            let sleeps = self.sleeps.take();
            self.sleeps.set(sleeps.clone());
            sleeps
        }
    }

    impl Clock for Virtual {
        fn now(&self) -> Duration {
            self.now.get()
        }
    }

    impl Sleep for Virtual {
        fn sleep(&mut self, duration: Duration) {
            let mut sleeps = self.sleeps.take();
            sleeps.push(duration);
            self.sleeps.set(sleeps);
            self.now.set(self.now.get() + duration);
        }
    }

    struct Scripted {
        records: Vec<ProcessRecord>,
        failures: u32,
        calls: u32,
        /// Simulates the real time a snapshot takes by moving the virtual clock.
        on_call: Option<Box<dyn FnMut()>>,
    }

    impl Scripted {
        fn new(records: Vec<ProcessRecord>, failures: u32) -> Self {
            Self {
                records,
                failures,
                calls: 0,
                on_call: None,
            }
        }
    }

    impl ProcessSource for Scripted {
        type Error = &'static str;

        fn snapshot(&mut self) -> Result<&[ProcessRecord], Self::Error> {
            self.calls += 1;
            if let Some(effect) = self.on_call.as_mut() {
                effect();
            }
            if self.failures > 0 {
                self.failures -= 1;
                return Err("transient");
            }
            Ok(&self.records)
        }
    }

    fn supervisor() -> Supervisor {
        Supervisor::new(
            Duration::ZERO,
            ProcessTarget::Name(ProcessName::new(b"app").unwrap()),
            NonZeroUsize::new(4).unwrap(),
        )
        .unwrap()
    }

    fn budget(samples: u32, duration: Option<Duration>) -> Budget {
        Budget::new(NonZeroU32::new(samples), duration).unwrap()
    }

    #[test]
    fn budget_requires_a_finite_limit() {
        assert_eq!(Budget::new(None, None), Err(BudgetError::Unbounded));
        assert_eq!(
            Budget::new(NonZeroU32::new(MAX_SAMPLES + 1), None),
            Err(BudgetError::TooManySamples)
        );
        assert_eq!(
            Budget::new(None, Some(MAX_DURATION + Duration::from_nanos(1))),
            Err(BudgetError::TooLong)
        );
        assert_eq!(
            Budget::new(None, Some(Duration::ZERO)),
            Err(BudgetError::TooLong)
        );
        assert!(Budget::new(NonZeroU32::new(1), Some(MAX_DURATION)).is_ok());
    }

    #[test]
    fn samples_are_delivered_on_presence_deadlines_and_acknowledged() {
        let mut runtime = Virtual::new();
        let mut source = Scripted::new(
            vec![ProcessRecord::parse_stat(&fixture(5, b"app", b'S', 1)).unwrap()],
            0,
        );
        let mut supervisor = supervisor();
        let mut seen = Vec::new();
        let stop = run(
            &mut supervisor,
            &runtime.clone(),
            &mut source,
            budget(3, None),
            &mut runtime,
            |sample| seen.push(sample),
        )
        .unwrap();
        assert_eq!(stop, Stop::SampleBudget);
        assert_eq!(runtime.sleeps(), vec![PRESENCE_INTERVAL; 3]);
        assert_eq!(seen.len(), 3);
        for (index, sample) in seen.iter().enumerate() {
            let Sample::Delivered { at, delivery } = sample else {
                panic!("unexpected {sample:?}");
            };
            assert_eq!(*at, PRESENCE_INTERVAL * (index as u32 + 1));
            assert_eq!(delivery.id.get(), index as u64 + 1);
            assert_eq!(delivery.effect, Effect::None);
            assert_eq!(
                delivery.event,
                WatchdogEvent::PresenceChecked {
                    present: true,
                    consecutive_missed_ms: 0
                }
            );
        }
        assert_eq!(supervisor.pending(), None);
        assert_eq!(source.calls, 3);
    }

    #[test]
    fn duration_budget_caps_the_final_sleep_and_stops_without_a_sample() {
        let mut runtime = Virtual::new();
        let mut source = Scripted::new(Vec::new(), 0);
        let mut count = 0;
        let stop = run(
            &mut supervisor(),
            &runtime.clone(),
            &mut source,
            budget(100, Some(Duration::from_millis(1_200))),
            &mut runtime,
            |_| count += 1,
        )
        .unwrap();
        assert_eq!(stop, Stop::DurationBudget);
        assert_eq!(count, 2);
        assert_eq!(
            runtime.sleeps(),
            vec![
                PRESENCE_INTERVAL,
                PRESENCE_INTERVAL,
                Duration::from_millis(200)
            ]
        );
    }

    #[test]
    fn transient_failures_are_reported_counted_and_retried() {
        let mut runtime = Virtual::new();
        let mut source = Scripted::new(Vec::new(), 2);
        let mut supervisor = supervisor();
        let mut seen = Vec::new();
        let stop = run(
            &mut supervisor,
            &runtime.clone(),
            &mut source,
            budget(3, None),
            &mut runtime,
            |sample| seen.push(sample),
        )
        .unwrap();
        assert_eq!(stop, Stop::SampleBudget);
        assert!(matches!(
            seen[0],
            Sample::Failed {
                error: SupervisorError::Source("transient"),
                ..
            }
        ));
        assert!(matches!(seen[1], Sample::Failed { .. }));
        let Sample::Delivered { at, delivery } = &seen[2] else {
            panic!("expected delivery");
        };
        // The due callback waited through two retries; no misses were fabricated for them.
        assert_eq!(*at, Duration::from_millis(1_500));
        assert_eq!(
            delivery.event,
            WatchdogEvent::PresenceChecked {
                present: false,
                consecutive_missed_ms: 500
            }
        );
        assert_eq!(runtime.sleeps(), vec![PRESENCE_INTERVAL; 3]);
    }

    #[test]
    fn terminal_state_ends_the_run_after_the_reboot_intent_is_reported() {
        let mut runtime = Virtual::new();
        let mut source = Scripted::new(Vec::new(), 0);
        let mut supervisor = supervisor();
        let mut effects = Vec::new();
        let stop = run(
            &mut supervisor,
            &runtime.clone(),
            &mut source,
            budget(MAX_SAMPLES, None),
            &mut runtime,
            |sample| {
                if let Sample::Delivered { delivery, .. } = sample {
                    effects.push(delivery.effect);
                }
            },
        )
        .unwrap();
        assert_eq!(stop, Stop::Terminal);
        assert_eq!(effects.last(), Some(&Effect::RebootIntent));
        assert_eq!(effects.len(), 12);
        assert_eq!(runtime.now(), Duration::from_secs(9));
        assert_eq!(supervisor.next_deadline(), None);
    }

    #[test]
    fn clock_rollback_aborts_the_run() {
        let runtime = Virtual::new();
        let mut rolled = runtime.clone();
        let mut source = Scripted::new(Vec::new(), 0);
        let mut supervisor = Supervisor::new(
            Duration::from_secs(10),
            ProcessTarget::Name(ProcessName::new(b"app").unwrap()),
            NonZeroUsize::new(4).unwrap(),
        )
        .unwrap();
        let result = run(
            &mut supervisor,
            &runtime,
            &mut source,
            budget(1, None),
            &mut rolled,
            |_| {},
        );
        assert!(matches!(result, Err(ClockError::Rollback { .. })));
        assert_eq!(source.calls, 0);
    }

    /// Review 2026-10-08: the sleep was budgeted from a reading taken before the
    /// source ran, so a slow source added a sleep after the budget had expired.
    #[test]
    fn slow_source_work_is_charged_to_the_duration_budget_before_sleeping() {
        let mut runtime = Virtual::new();
        let clock = runtime.clone();
        let mut source = Scripted::new(Vec::new(), u32::MAX);
        source.on_call = Some(Box::new(move || {
            clock.now.set(clock.now.get() + Duration::from_millis(400));
        }));
        let mut seen = Vec::new();
        let stop = run(
            &mut supervisor(),
            &runtime.clone(),
            &mut source,
            budget(100, Some(Duration::from_millis(800))),
            &mut runtime,
            |sample| seen.push(sample),
        )
        .unwrap();
        assert_eq!(stop, Stop::DurationBudget);
        // One sleep to the first deadline; the 400 ms snapshot then spent the budget.
        assert_eq!(runtime.sleeps(), vec![PRESENCE_INTERVAL]);
        assert_eq!(runtime.now(), Duration::from_millis(900));
        assert_eq!(seen.len(), 1);
        assert!(matches!(
            seen[0],
            Sample::Failed { at, .. } if at == PRESENCE_INTERVAL
        ));
    }

    #[test]
    fn slow_source_shortens_the_next_sleep_when_budget_remains() {
        let mut runtime = Virtual::new();
        let clock = runtime.clone();
        let mut source = Scripted::new(Vec::new(), u32::MAX);
        source.on_call = Some(Box::new(move || {
            clock.now.set(clock.now.get() + Duration::from_millis(400));
        }));
        let stop = run(
            &mut supervisor(),
            &runtime.clone(),
            &mut source,
            budget(100, Some(Duration::from_millis(1_000))),
            &mut runtime,
            |_| {},
        )
        .unwrap();
        assert_eq!(stop, Stop::DurationBudget);
        // 500 to the deadline, snapshot to 900, then only the 100 ms left, not 500.
        assert_eq!(
            runtime.sleeps(),
            vec![PRESENCE_INTERVAL, Duration::from_millis(100)]
        );
        assert_eq!(runtime.now(), Duration::from_secs(1));
    }

    #[test]
    fn clock_rollback_during_work_aborts_even_on_the_final_sample() {
        for samples in [1, 10] {
            for during_source in [false, true] {
                let mut runtime = Virtual::new();
                let source_clock = runtime.clone();
                let sink_clock = runtime.clone();
                let mut source = Scripted::new(Vec::new(), 0);
                if during_source {
                    source.on_call = Some(Box::new(move || {
                        source_clock.now.set(Duration::from_millis(100));
                    }));
                }
                let result = run(
                    &mut supervisor(),
                    &runtime.clone(),
                    &mut source,
                    budget(samples, None),
                    &mut runtime,
                    |_| {
                        if !during_source {
                            sink_clock.now.set(Duration::from_millis(100));
                        }
                    },
                );
                assert!(matches!(
                    result,
                    Err(ClockError::Rollback { previous, now })
                        if previous == PRESENCE_INTERVAL && now == Duration::from_millis(100)
                ));
                assert_eq!(runtime.sleeps(), vec![PRESENCE_INTERVAL]);
            }
        }
    }
}

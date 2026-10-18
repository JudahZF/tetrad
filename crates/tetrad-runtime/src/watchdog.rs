use std::{fmt, time::Duration};

pub const PRESENCE_INTERVAL: Duration = Duration::from_millis(500);
pub const MISSED_CHECK_THRESHOLD_MS: u16 = 3_000;
pub const COUNTDOWN_INTERVAL: Duration = Duration::from_secs(1);
pub const INITIAL_COUNTDOWN_SECONDS: u8 = 5;

pub trait Clock {
    fn now(&self) -> Duration;
}

/// Presence is a process-exists observation, not an application heartbeat.
/// An observation error must not be converted into an absent process.
pub trait ProcessPresence {
    type Error;
    fn is_present(&mut self) -> Result<bool, Self::Error>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClockError {
    Rollback { previous: Duration, now: Duration },
    Overflow,
}

impl fmt::Display for ClockError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Rollback { previous, now } => {
                write!(f, "clock moved backward from {previous:?} to {now:?}")
            }
            Self::Overflow => f.write_str("timer deadline overflow"),
        }
    }
}
impl std::error::Error for ClockError {}

#[derive(Debug, PartialEq, Eq)]
pub enum WatchdogError<E> {
    Clock(ClockError),
    Presence(E),
}

impl<E: fmt::Display> fmt::Display for WatchdogError<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Clock(e) => e.fmt(f),
            Self::Presence(e) => write!(f, "process presence observation failed: {e}"),
        }
    }
}
impl<E: std::error::Error + 'static> std::error::Error for WatchdogError<E> {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Clock(e) => Some(e),
            Self::Presence(e) => Some(e),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WatchdogState {
    Monitoring {
        consecutive_missed_ms: u16,
    },
    /// The displayed value at the next tick; zero selects the reboot path.
    Countdown {
        next_seconds: u8,
    },
    RebootRequested,
    Cancelled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WatchdogEvent {
    PresenceChecked {
        present: bool,
        consecutive_missed_ms: u16,
    },
    CountdownStarted {
        seconds: u8,
    },
    CountdownTick {
        seconds: u8,
    },
    /// Conceptual output only. This crate never runs a reboot command.
    RebootRequested,
    Cancelled,
}

#[derive(Debug)]
pub struct Watchdog {
    state: WatchdogState,
    last_observed: Duration,
    next_deadline: Option<Duration>,
}

impl Watchdog {
    pub fn new(start: Duration) -> Result<Self, ClockError> {
        Ok(Self {
            state: WatchdogState::Monitoring {
                consecutive_missed_ms: 0,
            },
            last_observed: start,
            next_deadline: Some(
                start
                    .checked_add(PRESENCE_INTERVAL)
                    .ok_or(ClockError::Overflow)?,
            ),
        })
    }

    pub fn state(&self) -> WatchdogState {
        self.state
    }
    pub fn next_deadline(&self) -> Option<Duration> {
        self.next_deadline
    }

    /// Service at most one due timer callback. Late calls do not fabricate
    /// historical presence observations or replay missed countdown callbacks.
    /// The next callback is scheduled relative to this successful observation.
    pub fn poll<C: Clock, P: ProcessPresence>(
        &mut self,
        clock: &C,
        presence: &mut P,
    ) -> Result<Option<WatchdogEvent>, WatchdogError<P::Error>> {
        let Some(deadline) = self.next_deadline else {
            return Ok(None);
        };
        let now = clock.now();
        if now < self.last_observed {
            return Err(WatchdogError::Clock(ClockError::Rollback {
                previous: self.last_observed,
                now,
            }));
        }
        // Accepted clock readings remain the watermark even if the observer
        // fails. Presence counters and due state still change only on success.
        self.last_observed = now;
        if now < deadline {
            return Ok(None);
        }
        let (state, interval, event) = match self.state {
            WatchdogState::Monitoring {
                consecutive_missed_ms,
            } => {
                // Validate the possible deadline before calling the injected observer.
                now.checked_add(COUNTDOWN_INTERVAL)
                    .ok_or(WatchdogError::Clock(ClockError::Overflow))?;
                let present = presence.is_present().map_err(WatchdogError::Presence)?;
                let missed = if present {
                    0
                } else {
                    consecutive_missed_ms + 500
                };
                if missed >= MISSED_CHECK_THRESHOLD_MS {
                    (
                        WatchdogState::Countdown {
                            next_seconds: INITIAL_COUNTDOWN_SECONDS,
                        },
                        Some(COUNTDOWN_INTERVAL),
                        WatchdogEvent::CountdownStarted {
                            seconds: INITIAL_COUNTDOWN_SECONDS,
                        },
                    )
                } else {
                    (
                        WatchdogState::Monitoring {
                            consecutive_missed_ms: missed,
                        },
                        Some(PRESENCE_INTERVAL),
                        WatchdogEvent::PresenceChecked {
                            present,
                            consecutive_missed_ms: missed,
                        },
                    )
                }
            }
            WatchdogState::Countdown { next_seconds: 0 } => (
                WatchdogState::RebootRequested,
                None,
                WatchdogEvent::RebootRequested,
            ),
            WatchdogState::Countdown { next_seconds } => (
                WatchdogState::Countdown {
                    next_seconds: next_seconds - 1,
                },
                Some(COUNTDOWN_INTERVAL),
                WatchdogEvent::CountdownTick {
                    seconds: next_seconds,
                },
            ),
            WatchdogState::RebootRequested | WatchdogState::Cancelled => return Ok(None),
        };
        let next = interval
            .map(|i| {
                now.checked_add(i)
                    .ok_or(WatchdogError::Clock(ClockError::Overflow))
            })
            .transpose()?;
        self.state = state;
        self.next_deadline = next;
        Ok(Some(event))
    }

    /// Explicit offline shutdown policy. Stock recovery does not cancel an
    /// armed countdown when the process returns; UI cancellation is unresolved.
    pub fn cancel(&mut self) -> Option<WatchdogEvent> {
        if self.next_deadline.take().is_some() {
            self.state = WatchdogState::Cancelled;
            Some(WatchdogEvent::Cancelled)
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failed_presence_observation_does_not_erase_the_clock_watermark() {
        let mut watchdog = Watchdog::new(Duration::ZERO).unwrap();
        let mut presence = Presence {
            present: false,
            calls: 0,
            fail: true,
        };
        assert_eq!(
            watchdog.poll(&Time(Duration::from_secs(10)), &mut presence),
            Err(WatchdogError::Presence("synthetic failure"))
        );
        assert_eq!(
            watchdog.state(),
            WatchdogState::Monitoring {
                consecutive_missed_ms: 0
            }
        );
        assert_eq!(watchdog.next_deadline(), Some(PRESENCE_INTERVAL));
        presence.fail = false;
        assert_eq!(
            watchdog.poll(&Time(PRESENCE_INTERVAL), &mut presence),
            Err(WatchdogError::Clock(ClockError::Rollback {
                previous: Duration::from_secs(10),
                now: PRESENCE_INTERVAL
            }))
        );
        assert_eq!(presence.calls, 1);
        assert_eq!(
            watchdog
                .poll(&Time(Duration::from_secs(10)), &mut presence)
                .unwrap(),
            Some(WatchdogEvent::PresenceChecked {
                present: false,
                consecutive_missed_ms: 500
            })
        );
    }

    struct Time(Duration);
    impl Clock for Time {
        fn now(&self) -> Duration {
            self.0
        }
    }
    struct Presence {
        present: bool,
        calls: usize,
        fail: bool,
    }
    impl ProcessPresence for Presence {
        type Error = &'static str;
        fn is_present(&mut self) -> Result<bool, Self::Error> {
            self.calls += 1;
            if self.fail {
                Err("synthetic failure")
            } else {
                Ok(self.present)
            }
        }
    }
    fn presence() -> Presence {
        Presence {
            present: false,
            calls: 0,
            fail: false,
        }
    }
    fn poll(w: &mut Watchdog, p: &mut Presence, ms: u64) -> Option<WatchdogEvent> {
        w.poll(&Time(Duration::from_millis(ms)), p).unwrap()
    }
    fn armed() -> (Watchdog, Presence) {
        let mut w = Watchdog::new(Duration::ZERO).unwrap();
        let mut p = presence();
        for ms in (500..=3_000).step_by(500) {
            poll(&mut w, &mut p, ms);
        }
        (w, p)
    }

    #[test]
    fn presence_and_countdown_boundaries_match_pinned_callbacks() {
        let mut w = Watchdog::new(Duration::ZERO).unwrap();
        let mut p = presence();
        assert_eq!(poll(&mut w, &mut p, 499), None);
        for ms in (500..=2_500).step_by(500) {
            assert_eq!(
                poll(&mut w, &mut p, ms),
                Some(WatchdogEvent::PresenceChecked {
                    present: false,
                    consecutive_missed_ms: ms as u16
                })
            );
        }
        assert_eq!(poll(&mut w, &mut p, 2_999), None);
        assert_eq!(
            poll(&mut w, &mut p, 3_000),
            Some(WatchdogEvent::CountdownStarted { seconds: 5 })
        );
        assert_eq!(poll(&mut w, &mut p, 3_999), None);
        p.present = true; // No presence sampling or recovery cancellation once armed.
        for (ms, seconds) in [(4_000, 5), (5_000, 4), (6_000, 3), (7_000, 2), (8_000, 1)] {
            assert_eq!(
                poll(&mut w, &mut p, ms),
                Some(WatchdogEvent::CountdownTick { seconds })
            );
        }
        assert_eq!(poll(&mut w, &mut p, 8_999), None);
        assert_eq!(
            poll(&mut w, &mut p, 9_000),
            Some(WatchdogEvent::RebootRequested)
        );
        assert_eq!(poll(&mut w, &mut p, 99_000), None);
        assert_eq!(w.state(), WatchdogState::RebootRequested);
        assert_eq!(w.next_deadline(), None);
        assert_eq!(w.cancel(), None);
        assert_eq!(p.calls, 6);
    }

    #[test]
    fn successful_check_resets_consecutive_failures() {
        let mut w = Watchdog::new(Duration::ZERO).unwrap();
        let mut p = presence();
        for ms in (500..=2_500).step_by(500) {
            poll(&mut w, &mut p, ms);
        }
        p.present = true;
        assert_eq!(
            poll(&mut w, &mut p, 3_000),
            Some(WatchdogEvent::PresenceChecked {
                present: true,
                consecutive_missed_ms: 0
            })
        );
        p.present = false;
        for ms in (3_500..=5_500).step_by(500) {
            poll(&mut w, &mut p, ms);
        }
        assert_eq!(
            poll(&mut w, &mut p, 6_000),
            Some(WatchdogEvent::CountdownStarted { seconds: 5 })
        );
    }

    #[test]
    fn clock_rollback_and_observer_errors_leave_due_state_unchanged() {
        let mut w = Watchdog::new(Duration::ZERO).unwrap();
        let mut p = presence();
        poll(&mut w, &mut p, 499);
        assert!(matches!(
            w.poll(&Time(Duration::from_millis(498)), &mut p),
            Err(WatchdogError::Clock(ClockError::Rollback { .. }))
        ));
        assert_eq!(p.calls, 0);
        p.fail = true;
        assert_eq!(
            w.poll(&Time(Duration::from_millis(500)), &mut p),
            Err(WatchdogError::Presence("synthetic failure"))
        );
        assert_eq!(
            w.state(),
            WatchdogState::Monitoring {
                consecutive_missed_ms: 0
            }
        );
        assert_eq!(w.next_deadline(), Some(PRESENCE_INTERVAL));
        p.fail = false;
        assert!(poll(&mut w, &mut p, 500).is_some());
        assert!(Watchdog::new(Duration::MAX).is_err());
        let mut w = Watchdog::new(Duration::MAX - Duration::from_secs(2)).unwrap();
        let before = w.next_deadline();
        assert_eq!(
            w.poll(&Time(Duration::MAX), &mut p),
            Err(WatchdogError::Clock(ClockError::Overflow))
        );
        assert_eq!(w.next_deadline(), before);
    }

    #[test]
    fn late_polls_do_not_invent_past_observations() {
        let mut w = Watchdog::new(Duration::ZERO).unwrap();
        let mut p = presence();
        assert_eq!(
            poll(&mut w, &mut p, 30_000),
            Some(WatchdogEvent::PresenceChecked {
                present: false,
                consecutive_missed_ms: 500
            })
        );
        assert_eq!(p.calls, 1);
        assert_eq!(w.next_deadline(), Some(Duration::from_millis(30_500)));
        let (mut w, mut p) = armed();
        assert_eq!(
            poll(&mut w, &mut p, 60_000),
            Some(WatchdogEvent::CountdownTick { seconds: 5 })
        );
        assert_eq!(w.next_deadline(), Some(Duration::from_millis(61_000)));
    }

    #[test]
    fn explicit_cancellation_stops_checks_and_countdown() {
        for countdown in [false, true] {
            let (mut w, mut p) = if countdown {
                armed()
            } else {
                (Watchdog::new(Duration::ZERO).unwrap(), presence())
            };
            let calls = p.calls;
            assert_eq!(w.cancel(), Some(WatchdogEvent::Cancelled));
            assert_eq!(w.cancel(), None);
            assert_eq!(poll(&mut w, &mut p, 90_000), None);
            assert_eq!(p.calls, calls);
            assert_eq!(w.state(), WatchdogState::Cancelled);
        }
    }
}

//! Hardware watchdog feeder. Stock ZenWatchdog never opens `/dev/watchdog`;
//! `S15watchdog` runs BusyBox v1.23.1 `watchdog -t 5 /dev/watchdog` instead.
//! The device is an injected backend; this module only decides when to act.

use std::time::Duration;

use tetrad_runtime::watchdog::{Clock, ClockError};
use thiserror::Error;

/// BusyBox applet behavior. `-t 5` is confirmed (`S15watchdog:9`); the 60 s
/// default `-T` and the command order are inferred from upstream 1.23 source
/// and its `Ft:T:`, `WDIOC_SETOPTIONS` and `WDIOC_SETTIMEOUT` strings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FeederConfig {
    pub device: &'static str,
    pub feed_interval: Duration,
    pub hardware_timeout_seconds: u32,
}

impl FeederConfig {
    pub const STOCK: Self = Self {
        device: "/dev/watchdog",
        feed_interval: Duration::from_secs(5),
        hardware_timeout_seconds: 60,
    };
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WatchdogCommand {
    /// `WDIOC_SETOPTIONS` with `WDIOS_ENABLECARD`.
    EnableCard,
    /// `WDIOC_SETTIMEOUT` in seconds.
    SetTimeout { seconds: u32 },
    /// Write one zero byte.
    Feed,
    /// Write `V` so a driver without `nowayout` disarms on close.
    MagicClose,
}

pub trait HardwareWatchdog {
    type Error;
    /// Open write-only. Stock exits when this fails.
    fn open(&mut self, device: &str) -> Result<(), Self::Error>;
    fn apply(&mut self, command: WatchdogCommand) -> Result<(), Self::Error>;
}

#[derive(Debug, PartialEq, Eq, Error)]
pub enum FeederError<E> {
    #[error("{0}")]
    Clock(ClockError),
    #[error("watchdog device failed: {0}")]
    Device(E),
}

#[derive(Debug, PartialEq, Eq)]
pub enum Feed<E> {
    NotDue,
    Fed,
    /// Stock ignores the `write` result and keeps its schedule.
    Failed(E),
}

/// A command that failed without stopping the feeder.
#[derive(Debug, PartialEq, Eq)]
pub struct Warning<E> {
    pub command: WatchdogCommand,
    pub error: E,
}

#[derive(Debug)]
pub struct Started<E> {
    pub feeder: Feeder,
    pub warnings: Vec<Warning<E>>,
}

#[derive(Debug)]
pub struct Feeder {
    interval: Duration,
    last_observed: Duration,
    next_feed: Duration,
}

impl Feeder {
    /// Open, enable, set the timeout and feed once. `ioctl_or_warn` makes the
    /// two configuration failures warnings, so they are returned, not fatal.
    pub fn start<C: Clock, W: HardwareWatchdog>(
        config: FeederConfig,
        clock: &C,
        device: &mut W,
    ) -> Result<Started<W::Error>, FeederError<W::Error>> {
        device.open(config.device).map_err(FeederError::Device)?;
        let mut warnings = Vec::new();
        for command in [
            WatchdogCommand::EnableCard,
            WatchdogCommand::SetTimeout {
                seconds: config.hardware_timeout_seconds,
            },
        ] {
            if let Err(error) = device.apply(command) {
                warnings.push(Warning { command, error });
            }
        }
        let now = clock.now();
        let mut feeder = Self {
            interval: config.feed_interval,
            last_observed: now,
            next_feed: now,
        };
        if let Feed::Failed(error) = feeder.poll(clock, device)? {
            warnings.push(Warning {
                command: WatchdogCommand::Feed,
                error,
            });
        }
        Ok(Started { feeder, warnings })
    }

    pub fn next_deadline(&self) -> Duration {
        self.next_feed
    }

    /// Feed when due; the next feed is one interval after this one.
    pub fn poll<C: Clock, W: HardwareWatchdog>(
        &mut self,
        clock: &C,
        device: &mut W,
    ) -> Result<Feed<W::Error>, FeederError<W::Error>> {
        let now = clock.now();
        if now < self.last_observed {
            return Err(FeederError::Clock(ClockError::Rollback {
                previous: self.last_observed,
                now,
            }));
        }
        self.last_observed = now;
        if now < self.next_feed {
            return Ok(Feed::NotDue);
        }
        self.next_feed = now
            .checked_add(self.interval)
            .ok_or(FeederError::Clock(ClockError::Overflow))?;
        Ok(match device.apply(WatchdogCommand::Feed) {
            Ok(()) => Feed::Fed,
            Err(error) => Feed::Failed(error),
        })
    }

    /// Stock writes the magic character on a fatal signal (init's shutdown
    /// `SIGTERM`), then exits. Consumes the feeder: nothing feeds afterward.
    pub fn stop<W: HardwareWatchdog>(self, device: &mut W) -> Result<(), W::Error> {
        device.apply(WatchdogCommand::MagicClose)
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use super::*;

    struct Time(Cell<Duration>);
    impl Clock for Time {
        fn now(&self) -> Duration {
            self.0.get()
        }
    }

    #[derive(Default)]
    struct Device {
        log: Vec<WatchdogCommand>,
        refuse_timeout: bool,
    }
    impl HardwareWatchdog for Device {
        type Error = &'static str;
        fn open(&mut self, device: &str) -> Result<(), Self::Error> {
            assert_eq!(device, "/dev/watchdog");
            Ok(())
        }
        fn apply(&mut self, command: WatchdogCommand) -> Result<(), Self::Error> {
            self.log.push(command);
            match command {
                WatchdogCommand::SetTimeout { .. } if self.refuse_timeout => Err("EINVAL"),
                _ => Ok(()),
            }
        }
    }

    #[test]
    fn stock_feeder_configures_feeds_every_five_seconds_and_closes_with_magic() {
        let time = Time(Cell::new(Duration::from_secs(10)));
        let mut device = Device {
            refuse_timeout: true,
            ..Device::default()
        };
        let Started {
            mut feeder,
            warnings,
        } = Feeder::start(FeederConfig::STOCK, &time, &mut device).unwrap();
        assert_eq!(
            warnings,
            [Warning {
                command: WatchdogCommand::SetTimeout { seconds: 60 },
                error: "EINVAL",
            }]
        );
        assert_eq!(feeder.next_deadline(), Duration::from_secs(15));
        time.0.set(Duration::from_millis(14_999));
        assert_eq!(feeder.poll(&time, &mut device), Ok(Feed::NotDue));
        time.0.set(Duration::from_secs(16));
        assert_eq!(feeder.poll(&time, &mut device), Ok(Feed::Fed));
        assert_eq!(feeder.next_deadline(), Duration::from_secs(21));
        time.0.set(Duration::from_secs(1));
        assert!(matches!(
            feeder.poll(&time, &mut device),
            Err(FeederError::Clock(ClockError::Rollback { .. }))
        ));
        feeder.stop(&mut device).unwrap();
        assert_eq!(
            device.log,
            [
                WatchdogCommand::EnableCard,
                WatchdogCommand::SetTimeout { seconds: 60 },
                WatchdogCommand::Feed,
                WatchdogCommand::Feed,
                WatchdogCommand::MagicClose,
            ]
        );
    }
}

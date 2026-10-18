//! Stock QC lifecycle: startup, supervision, recovery and power actions. Every
//! output is a typed intent; injected backends execute them. Nothing here kills,
//! reboots, unmounts, mounts or opens a device by itself.
//!
//! - [`Supervisor`]: ZenWatchdog presence/countdown model (timers from
//!   `tetrad-runtime`); process selection and acknowledged delivery are owned policy.
//! - [`recovery`]: stock crash screen, log lines and reboot-helper launch per delivery.
//! - [`boot`]: `S80drvinit` start order, log rotation and splash handshake.
//! - [`splash`]: `/splashqueue` command codec and `splashqueue` CLI semantics.
//! - [`power`]: ZenUI power-off/restart, `zenreboot`, power-button events.
//! - [`hwwatchdog`]: BusyBox `/dev/watchdog` feeder schedule.
//! - [`cmdline`]: the four stock `/proc/cmdline` product parsers.
//! - [`procfs`] and [`monitor`]: bounded Linux `/proc` reader and finite loop.
//!
//! ```
//! use std::{num::NonZeroUsize, time::Duration};
//! use tetrad_watchdog::{Clock, Effect, Supervisor};
//! use tetrad_watchdog::process::{ProcessName, ProcessRecord, ProcessSource, ProcessTarget};
//!
//! struct Time(Duration);
//! impl Clock for Time {
//!     fn now(&self) -> Duration { self.0 }
//! }
//! struct Empty;
//! impl ProcessSource for Empty {
//!     type Error = std::convert::Infallible;
//!     fn snapshot(&mut self) -> Result<&[ProcessRecord], Self::Error> { Ok(&[]) }
//! }
//! let target = ProcessTarget::Name(ProcessName::new(b"app")?);
//! let mut supervisor = Supervisor::new(Duration::ZERO, target, NonZeroUsize::new(64).unwrap())?;
//! let mut source = Empty;
//! let delivery = *supervisor.poll(&Time(Duration::from_millis(500)), &mut source)?.unwrap();
//! assert_eq!(delivery.effect, Effect::None);
//! // Persist or accept the intent before acknowledging its instance-local ID.
//! supervisor.acknowledge(delivery.id)?;
//! assert_eq!(supervisor.next_deadline(), Some(Duration::from_millis(1_000)));
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
#![forbid(unsafe_code)]

pub mod boot;
pub mod cmdline;
pub mod hwwatchdog;
pub mod monitor;
pub mod power;
pub mod process;
pub mod procfs;
pub mod recovery;
pub mod splash;
mod supervisor;

pub use supervisor::{
    AcknowledgeError, CancelError, Delivery, DeliveryId, Effect, Supervisor, SupervisorError,
};
pub use tetrad_runtime::watchdog::{Clock, ClockError, WatchdogEvent, WatchdogState};

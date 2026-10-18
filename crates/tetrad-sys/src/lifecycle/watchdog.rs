//! `/dev/watchdog` for `tetrad_watchdog::hwwatchdog::Feeder`. Stock is BusyBox v1.23.1
//! `watchdog -t 5 /dev/watchdog` (`S15watchdog:9`). Request numbers: ADI sysroot `linux/watchdog.h:27-29`;
//! BusyBox keeps `WDIOC_SETOPTIONS` and `WDIOC_SETTIMEOUT` in its literal pool (file offsets `0xa204`,
//! `0xa214`) and has no `WDIOC_KEEPALIVE`.

use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::os::fd::AsRawFd;

use tetrad_watchdog::hwwatchdog::{HardwareWatchdog, WatchdogCommand};
use thiserror::Error;

/// `_IOR('W', 4, int)`.
pub const WDIOC_SETOPTIONS: u32 = 0x8004_5704;
/// `_IOR('W', 5, int)`. The argument is ignored by the kernel.
pub const WDIOC_KEEPALIVE: u32 = 0x8004_5705;
/// `_IOWR('W', 6, int)`: seconds in, the timeout the driver applied out.
pub const WDIOC_SETTIMEOUT: u32 = 0xc004_5706;
pub const WDIOS_ENABLECARD: libc::c_int = 0x0002;
/// Written before close so a driver without `nowayout` stops the timer on release.
pub const MAGIC_CLOSE: u8 = b'V';

#[derive(Debug, Error)]
pub enum WatchdogError {
    #[error("watchdog device is not open")]
    NotOpen,
    #[error("watchdog device is already open")]
    AlreadyOpen,
    #[error("timeout of {0} s does not fit a C int")]
    TimeoutRange(u32),
    #[error("driver reported a negative timeout {0}")]
    NegativeTimeout(libc::c_int),
    #[error(transparent)]
    Io(#[from] io::Error),
}

/// One watchdog descriptor. Dropping it without [`WatchdogCommand::MagicClose`] closes without the magic byte:
/// the timer keeps running and resets the board unless something else feeds it. That is the safety contract.
#[derive(Debug, Default)]
pub struct LinuxWatchdog {
    file: Option<File>,
    applied_timeout: Option<u32>,
}

impl LinuxWatchdog {
    fn file(&self) -> Result<&File, WatchdogError> {
        self.file.as_ref().ok_or(WatchdogError::NotOpen)
    }

    /// One `int`-argument watchdog ioctl; returns the word as the driver left it.
    fn ioctl_int(&self, request: u32, value: libc::c_int) -> Result<libc::c_int, WatchdogError> {
        let fd = self.file()?.as_raw_fd();
        let mut word = value;
        // SAFETY: every request used here is `_IOR`/`_IOWR` of one `int`; `word` is a live, exclusively
        // borrowed `int`, so the kernel copies at most four bytes in or out. `fd` stays open through `self`.
        let result = unsafe { libc::ioctl(fd, request as libc::Ioctl, &mut word) };
        if result < 0 {
            return Err(io::Error::last_os_error().into());
        }
        Ok(word)
    }

    /// `WDIOC_SETTIMEOUT`. Returns the timeout the driver applied; drivers may round or clamp it.
    pub fn set_timeout(&mut self, seconds: u32) -> Result<u32, WatchdogError> {
        let requested =
            libc::c_int::try_from(seconds).map_err(|_| WatchdogError::TimeoutRange(seconds))?;
        let applied = self.ioctl_int(WDIOC_SETTIMEOUT, requested)?;
        let applied =
            u32::try_from(applied).map_err(|_| WatchdogError::NegativeTimeout(applied))?;
        self.applied_timeout = Some(applied);
        Ok(applied)
    }

    /// The last timeout the driver reported. The feed interval must stay well below it.
    pub fn applied_timeout(&self) -> Option<u32> {
        self.applied_timeout
    }

    /// Owned alternative to the stock zero-byte write. The kernel answers `EOPNOTSUPP` when the driver lacks
    /// `WDIOF_KEEPALIVEPING`.
    pub fn keepalive(&self) -> Result<(), WatchdogError> {
        self.ioctl_int(WDIOC_KEEPALIVE, 0).map(drop)
    }
}

impl HardwareWatchdog for LinuxWatchdog {
    type Error = WatchdogError;

    /// BusyBox `xopen(device, O_WRONLY)`. Rust std adds `O_CLOEXEC`, so no child keeps the timer open.
    fn open(&mut self, device: &str) -> Result<(), WatchdogError> {
        if self.file.is_some() {
            return Err(WatchdogError::AlreadyOpen);
        }
        self.file = Some(OpenOptions::new().write(true).open(device)?);
        self.applied_timeout = None;
        Ok(())
    }

    fn apply(&mut self, command: WatchdogCommand) -> Result<(), WatchdogError> {
        match command {
            WatchdogCommand::EnableCard => {
                self.ioctl_int(WDIOC_SETOPTIONS, WDIOS_ENABLECARD).map(drop)
            }
            WatchdogCommand::SetTimeout { seconds } => self.set_timeout(seconds).map(drop),
            // BusyBox `write(3, "", 1)`.
            WatchdogCommand::Feed => Ok(self.file()?.write_all(&[0])?),
            // BusyBox writes `V`, closes and exits; the close is what disarms. The descriptor closes even when
            // the write fails.
            WatchdogCommand::MagicClose => {
                let mut file = self.file.take().ok_or(WatchdogError::NotOpen)?;
                Ok(file.write_all(&[MAGIC_CLOSE])?)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::time::Duration;

    use tetrad_watchdog::Clock;
    use tetrad_watchdog::hwwatchdog::{Feed, Feeder, FeederConfig, Started};

    use super::*;

    struct Time(Cell<Duration>);
    impl Clock for Time {
        fn now(&self) -> Duration {
            self.0.get()
        }
    }

    /// The stock feeder over a regular file: both configuration ioctls fail with ENOTTY and stay warnings, feeds
    /// write zero bytes, and stop writes the magic byte and closes.
    #[test]
    fn stock_feeder_writes_zero_feeds_then_magic_close_on_a_file() {
        let dir = std::env::temp_dir().join(format!("tetrad-sys-wdt-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("watchdog");
        std::fs::write(&path, b"").unwrap();
        let device: &'static str = String::leak(path.to_str().unwrap().to_owned());

        let time = Time(Cell::new(Duration::from_secs(1)));
        let mut wdt = LinuxWatchdog::default();
        let config = FeederConfig {
            device,
            ..FeederConfig::STOCK
        };
        let Started {
            mut feeder,
            warnings,
        } = Feeder::start(config, &time, &mut wdt).unwrap();
        let failed: Vec<_> = warnings
            .iter()
            .map(|warning| match &warning.error {
                WatchdogError::Io(error) => (warning.command, error.raw_os_error()),
                other => panic!("unexpected {other:?}"),
            })
            .collect();
        assert_eq!(
            failed,
            [
                (WatchdogCommand::EnableCard, Some(libc::ENOTTY)),
                (
                    WatchdogCommand::SetTimeout { seconds: 60 },
                    Some(libc::ENOTTY)
                ),
            ]
        );
        assert_eq!(wdt.applied_timeout(), None);
        assert!(matches!(wdt.open(device), Err(WatchdogError::AlreadyOpen)));

        time.0.set(Duration::from_secs(6));
        assert!(matches!(feeder.poll(&time, &mut wdt), Ok(Feed::Fed)));
        feeder.stop(&mut wdt).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), [0, 0, MAGIC_CLOSE]);
        assert!(matches!(
            wdt.apply(WatchdogCommand::Feed),
            Err(WatchdogError::NotOpen)
        ));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}

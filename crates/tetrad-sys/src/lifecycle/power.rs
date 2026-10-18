//! Linux backend for `tetrad_watchdog::power` sequences. Evidence: ZenUI teardown `0x66a208` and restart
//! `0x66b0cc` (every step a `system()` call), zenreboot main `0x9580`, libzenhal `CZenjack::Open` `0x1d1a4`,
//! `Restart` `0x1cb7c`, `Shutdown` `0x1cbe0`, `Close` `0x19904`.

use std::process::{Command, Stdio};

use tetrad_hal::node::Node;
use tetrad_hal::zenjack::PowerCommand;
use tetrad_watchdog::power::{PowerBackend, PowerStep, StepStatus};
use thiserror::Error;

use super::{E2FSCK_ARGS, remount_read_only, run, sync, unmount};
use crate::ioctl::{Ioctl, IoctlNode};

/// Programs a power sequence runs. Names without `/` are found through `PATH`, as `system()` does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PowerPrograms {
    /// `killall tee` (stock) and `killall tail` (owned follower stop).
    pub killall: &'static str,
    pub e2fsck: &'static str,
    /// `system("/opt/neuraldsp/zenreboot")`; the sequence waits for it.
    pub reboot_helper: &'static str,
    /// `/bin/reboot` line 1.
    pub backlight_off: &'static str,
    /// `/bin/reboot` line 2: BusyBox `reboot` signals init.
    pub init_reboot: &'static str,
}

impl PowerPrograms {
    pub const STOCK: Self = Self {
        killall: "killall",
        e2fsck: "e2fsck",
        reboot_helper: "/opt/neuraldsp/zenreboot",
        backlight_off: "/opt/neuraldsp/backlightoff",
        init_reboot: "/sbin/reboot",
    };
}

/// The step needs application state: the DSP controller, headphone gain (`tetrad_hal::hal::StockHal`) or the
/// mini's LED controller (ZenUI `0x509b4c`/`0x50a44c`/`0x50c2a8`). The app handles it, then delegates the
/// rest to [`LinuxPower`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[error("power step {0:?} needs application state")]
pub struct NeedsApplication(pub PowerStep);

/// A program with stdin, stdout and stderr on `/dev/null`. Its output must not reach this process: after
/// `killall tee` the stdout and stderr of this process have no reader, and the child would die of SIGPIPE.
fn silent(program: &str) -> Command {
    let mut command = Command::new(program);
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    command
}

/// Runs every system step of a power sequence. A step that runs and fails is `StepStatus::Failure`, as
/// stock ignores each `system()` result.
///
/// Do not print to stdout or stderr after [`PowerStep::StopLogPipelines`]. `killall tee` leaves them as a pipe
/// with no reader: `println!` and `eprintln!` panic, and a `write` fails with `EPIPE`.
#[derive(Debug)]
pub struct LinuxPower {
    programs: PowerPrograms,
    soundcard: Option<IoctlNode>,
}

impl LinuxPower {
    /// No HAL node yet: the zenreboot sequence opens it with `PowerStep::HalOpen`.
    pub fn new(programs: PowerPrograms) -> Self {
        Self {
            programs,
            soundcard: None,
        }
    }

    /// ZenUI sequences assume the HAL is open. Pass the app's `/dev/zjack/soundcard` node, or a duplicate
    /// (`IoctlNode::from_fd(Node::Soundcard, node.as_fd().try_clone_to_owned()?)`).
    pub fn with_soundcard(programs: PowerPrograms, soundcard: IoctlNode) -> Self {
        Self {
            programs,
            soundcard: Some(soundcard),
        }
    }

    /// `CZenjack::Restart`/`Shutdown`: one ioctl with a zero word, only while open; success is a return `>= 0`.
    fn hal(&self, command: PowerCommand) -> bool {
        self.soundcard
            .as_ref()
            .and_then(|node| node.execute(command.request()).ok())
            .is_some_and(|reply| reply.raw_return >= 0)
    }
}

impl PowerBackend for LinuxPower {
    type Error = NeedsApplication;

    fn perform(&mut self, step: PowerStep) -> Result<StepStatus, NeedsApplication> {
        let programs = self.programs;
        let success = match step {
            PowerStep::Dsp(_) | PowerStep::MuteHeadphones | PowerStep::SurfaceLedsOff => {
                return Err(NeedsApplication(step));
            }
            PowerStep::Delay(duration) => {
                std::thread::sleep(duration);
                true
            }
            PowerStep::StopLogPipelines => run(silent(programs.killall).arg("tee")),
            PowerStep::StopLogFollower => run(silent(programs.killall).arg("tail")),
            PowerStep::Sync => {
                sync();
                true
            }
            PowerStep::Unmount(target) => unmount(target).is_ok(),
            PowerStep::RemountReadOnly(target) => remount_read_only(target).is_ok(),
            PowerStep::CheckFilesystem(device) => {
                run(silent(programs.e2fsck).args(E2FSCK_ARGS).arg(device))
            }
            // Owned: stock `CZenjack::Open` also opens the six event nodes and sets `_opened` only when all
            // seven open, so one missing event node makes its restart a silent no-op. Only this node is needed.
            PowerStep::HalOpen => {
                if self.soundcard.is_none() {
                    self.soundcard = IoctlNode::open(Node::Soundcard).ok();
                }
                self.soundcard.is_some()
            }
            PowerStep::HalClose => {
                self.soundcard = None;
                true
            }
            PowerStep::HalRestart => self.hal(PowerCommand::Restart),
            PowerStep::HalShutdown => self.hal(PowerCommand::Shutdown),
            PowerStep::RunRebootHelper => run(&mut silent(programs.reboot_helper)),
            PowerStep::BacklightOff => run(&mut silent(programs.backlight_off)),
            PowerStep::InitReboot => run(&mut silent(programs.init_reboot)),
        };
        Ok(if success {
            StepStatus::Success
        } else {
            StepStatus::Failure
        })
    }
}

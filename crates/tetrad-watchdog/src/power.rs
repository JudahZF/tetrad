//! Stock power-off, restart and reboot-helper sequences as typed steps, plus the
//! power-button decision. An injected [`PowerBackend`] executes each step.
//!
//! Evidence: ZenUI teardown `0x66a208` (power-off) and `0x66b0cc` (restart),
//! callers `0x6a87d4`, `0x6a8874`, `0x6a8914`; zenreboot main `0x9580`;
//! libzenhal `CZenjack::Shutdown` `0x1cbe0` and `Restart` `0x1cb7c`.

use std::time::Duration;

use crate::{
    boot::{USER_DATA_DEVICE, USER_DATA_MOUNT},
    cmdline::{KernelCmdline, Product, RestartMethod},
};

/// Stock pause after the last sync, before the hardware action (`usleep(10000)`).
pub const HAL_SETTLE: Duration = Duration::from_millis(10);
/// zenreboot `sleep(1)` after unmounting.
pub const HELPER_SETTLE: Duration = Duration::from_secs(1);

/// DSPController commands sent around power actions. Meanings are inferred.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DspPowerCommand {
    /// Command `0x20` to cores 0 and 1 with payload `a | b << 1`. Power actions
    /// send 0; standby entry sends 3 together with brightness 0.
    PowerFlags(u8),
    /// Command `0x5a`, sub-byte 4, after the HAL restart on the mini only.
    MiniRestart,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PowerStep {
    Dsp(DspPowerCommand),
    /// `CZenjack::SetHeadPhoneVolume(0.0, true, false, 3, false, 25000)`.
    MuteHeadphones,
    /// Mini only: four surface LEDs set to black, then flushed.
    SurfaceLedsOff,
    Delay(Duration),
    /// `killall tee`: ends the boot log pipelines so p4 files close. Stock is
    /// system-wide; an owned backend stops only its own log writers.
    StopLogPipelines,
    /// Owned addition: the boot `tail -f` follower also holds a p4 file open.
    /// Stock ZenUI never stops it; zensplash runs `killall tail`.
    StopLogFollower,
    Sync,
    Unmount(&'static str),
    /// Owned fallback when unmounting fails.
    RemountReadOnly(&'static str),
    /// `e2fsck -C0 -p -f -v <device>`.
    CheckFilesystem(&'static str),
    HalOpen,
    HalClose,
    /// ioctl `0x4004e025` on `/dev/zjack/soundcard`, payload zero `u32`.
    HalRestart,
    /// ioctl `0x4004e028` on `/dev/zjack/soundcard`, payload zero `u32`.
    HalShutdown,
    /// `system("/opt/neuraldsp/zenreboot")`; waits for [`reboot_helper`].
    RunRebootHelper,
    /// `/bin/reboot` part one: `/opt/neuraldsp/backlightoff`.
    BacklightOff,
    /// `/bin/reboot` part two: BusyBox `/sbin/reboot` signals init, which runs
    /// `rcK`, `umount -a -r` and `swapoff -a` before the kernel reboot.
    InitReboot,
}

/// Why ZenUI restarts. Only the steps before the teardown differ.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RestartReason {
    /// Power menu REBOOT (`0x6a8874`).
    PowerMenu,
    /// DSP fault or SOC2-prepared dialog (`0x6a8914`): mute, then sleep the
    /// requested seconds clamped to 1..=10. Stock passes 2.
    DspFault { delay_seconds: u32 },
    /// Update finalize (`0x424740`), settings restart dialog, cloud-endpoint
    /// change and production-test enable call the teardown directly.
    Direct,
}

/// The two stock steps before the teardown: DSP `0x20` value 0, then the headphones mute.
/// Stock code calls this "mute first", but deploy start and standby send value 1 for mute,
/// so the meaning of value 0 stays inferred (I) until bring-up stage S4a proves it.
const POWER_FLAGS_0: [PowerStep; 2] = [
    PowerStep::Dsp(DspPowerCommand::PowerFlags(0)),
    PowerStep::MuteHeadphones,
];

/// Teardown shared by `0x66a208` and `0x66b0cc`, ending before the HAL action.
const TEARDOWN: [PowerStep; 8] = [
    PowerStep::StopLogPipelines,
    PowerStep::Sync,
    PowerStep::Unmount(USER_DATA_MOUNT),
    PowerStep::Sync,
    PowerStep::CheckFilesystem(USER_DATA_DEVICE),
    PowerStep::Sync,
    PowerStep::Delay(HAL_SETTLE),
    PowerStep::HalShutdown,
];

/// Power menu SHUTDOWN (`0x6a87d4`), or the mini's long press. The caller has
/// already passed the stock gates (`0x1ec950` state check; on the full-size unit
/// also `0x64f464`), whose meanings are not recovered.
pub fn power_off() -> Vec<PowerStep> {
    [&POWER_FLAGS_0[..], &TEARDOWN[..]].concat()
}

/// ZenUI restart. On the mini the HAL restart is followed by the reboot helper.
/// `product` is ZenUI's mini test `0x499a60`: [`KernelCmdline::hal_product`], or
/// `Product::from(Platform)` when the emulation files apply.
pub fn restart(reason: RestartReason, product: Product) -> Vec<PowerStep> {
    let mut steps = match reason {
        RestartReason::PowerMenu => POWER_FLAGS_0.to_vec(),
        RestartReason::DspFault { delay_seconds } => {
            let mut steps = POWER_FLAGS_0.to_vec();
            let seconds = delay_seconds.clamp(1, 10);
            steps.push(PowerStep::Delay(Duration::from_secs(seconds.into())));
            steps
        }
        RestartReason::Direct => Vec::new(),
    };
    let mini = product == Product::QuadCortexMini;
    if mini {
        steps.push(PowerStep::SurfaceLedsOff);
    }
    steps.extend_from_slice(&TEARDOWN[..TEARDOWN.len() - 1]);
    steps.push(PowerStep::HalRestart);
    if mini {
        steps.push(PowerStep::Dsp(DspPowerCommand::MiniRestart));
        steps.push(PowerStep::RunRebootHelper);
    }
    steps
}

/// `zenreboot` (`0x9580`). `None` means `/proc/cmdline` could not be opened;
/// the default product ID `QC` then selects the HAL restart.
pub fn reboot_helper(cmdline: Option<KernelCmdline<'_>>) -> Vec<PowerStep> {
    let method = cmdline.map_or(RestartMethod::HalRestart, KernelCmdline::zenreboot_restart);
    let mut steps = vec![
        PowerStep::HalOpen,
        PowerStep::StopLogPipelines,
        PowerStep::Sync,
        PowerStep::Unmount(USER_DATA_MOUNT),
        PowerStep::Sync,
        PowerStep::Delay(HELPER_SETTLE),
    ];
    match method {
        RestartMethod::HalRestart => steps.push(PowerStep::HalRestart),
        RestartMethod::InitReboot => {
            steps.extend([PowerStep::BacklightOff, PowerStep::InitReboot]);
        }
    }
    steps.push(PowerStep::HalClose);
    steps
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepStatus {
    Success,
    Failure,
}

pub trait PowerBackend {
    type Error;
    /// `Err` means the step could not be attempted; it aborts the sequence.
    /// A step that ran and failed is [`StepStatus::Failure`].
    fn perform(&mut self, step: PowerStep) -> Result<StepStatus, Self::Error>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Policy {
    /// Ignore every result, as stock does.
    Stock,
    /// Owned: stop the log follower right after [`PowerStep::StopLogPipelines`]
    /// (unless the steps already do). When unmounting still fails, remount
    /// read-only and skip the filesystem check (it cannot run on a mounted
    /// volume). The power action still runs: data was synced and the user asked
    /// for power-off. The executing process must close its own p4 files first.
    Safe,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PowerReport {
    /// Steps that ran and failed, in order.
    pub failed: Vec<PowerStep>,
    /// Steps skipped by [`Policy::Safe`].
    pub skipped: Vec<PowerStep>,
}

/// A sequence stopped because a step could not be attempted. `report` holds what
/// ran before `step`, so the caller still knows which teardown steps failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PowerAbort<E> {
    pub step: PowerStep,
    pub error: E,
    pub report: PowerReport,
}

pub fn execute<B: PowerBackend>(
    steps: &[PowerStep],
    policy: Policy,
    backend: &mut B,
) -> Result<PowerReport, PowerAbort<B::Error>> {
    let safe = policy == Policy::Safe;
    let add_follower = safe && !steps.contains(&PowerStep::StopLogFollower);
    let mut report = PowerReport::default();
    let perform = |backend: &mut B, report: &mut PowerReport, step| match backend.perform(step) {
        Ok(StepStatus::Success) => Ok(true),
        Ok(StepStatus::Failure) => {
            report.failed.push(step);
            Ok(false)
        }
        Err(error) => Err((step, error)),
    };
    let run = |backend: &mut B, report: &mut PowerReport| {
        let mut still_mounted = false;
        for &step in steps {
            if safe && still_mounted && matches!(step, PowerStep::CheckFilesystem(_)) {
                report.skipped.push(step);
                continue;
            }
            let ok = perform(backend, report, step)?;
            match step {
                PowerStep::StopLogPipelines if add_follower => {
                    perform(backend, report, PowerStep::StopLogFollower)?;
                }
                PowerStep::Unmount(target) if safe && !ok => {
                    still_mounted = true;
                    perform(backend, report, PowerStep::RemountReadOnly(target))?;
                }
                _ => {}
            }
        }
        Ok(())
    };
    match run(backend, &mut report) {
        Ok(()) => Ok(report),
        Err((step, error)) => Err(PowerAbort {
            step,
            error,
            report,
        }),
    }
}

/// Long-press timer control. Its timeout calls `shutdownPressed`, so the
/// mini powers off on a long press. The stock interval is not recovered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LongPress {
    Start,
    Stop,
    Keep,
}

/// One normalized power-button event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ButtonEvent {
    /// The stock `newPowerOnOffEventRead(bool)` value.
    pub pressed: bool,
    pub long_press: LongPress,
}

/// `CZenjack::PowerButtonEvent` (`0x1ca38`) reads one `EV_KEY`/`KEY_POWER`
/// value; `PowerOnOffThread` (`0x6a80d4`) signals "pressed" for value 1 on the
/// mini and value 0 on the full-size unit. Slot `0x6a852c` stops the long-press
/// timer on a mini non-press and starts it on a mini press; on the full-size
/// unit it returns before the timer. `product` comes from the same source as for
/// [`restart`]. The slot's own gate `0x5447e0` and the power menu it may show
/// depend on UI state owned by the UI.
pub fn power_button(product: Product, value: i32) -> ButtonEvent {
    let pressed = match product {
        Product::QuadCortexMini => value == 1,
        Product::QuadCortex => value == 0,
    };
    let long_press = match (product, pressed) {
        (Product::QuadCortexMini, true) => LongPress::Start,
        (Product::QuadCortexMini, false) => LongPress::Stop,
        (Product::QuadCortex, _) => LongPress::Keep,
    };
    ButtonEvent {
        pressed,
        long_press,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Busy {
        log: Vec<PowerStep>,
    }
    impl PowerBackend for Busy {
        type Error = std::convert::Infallible;
        fn perform(&mut self, step: PowerStep) -> Result<StepStatus, Self::Error> {
            self.log.push(step);
            Ok(match step {
                PowerStep::Unmount(_) => StepStatus::Failure,
                _ => StepStatus::Success,
            })
        }
    }

    #[test]
    fn stock_sequences_mute_first_and_end_with_the_recovered_hardware_action() {
        let off = power_off();
        assert_eq!(off[..2], POWER_FLAGS_0);
        assert_eq!(off.last(), Some(&PowerStep::HalShutdown));
        assert_eq!(
            restart(
                RestartReason::DspFault { delay_seconds: 0 },
                Product::QuadCortex
            )[2],
            PowerStep::Delay(Duration::from_secs(1))
        );
        let mini = restart(RestartReason::Direct, Product::QuadCortexMini);
        assert_eq!(mini.first(), Some(&PowerStep::SurfaceLedsOff));
        assert_eq!(
            mini[mini.len() - 3..],
            [
                PowerStep::HalRestart,
                PowerStep::Dsp(DspPowerCommand::MiniRestart),
                PowerStep::RunRebootHelper,
            ]
        );
        let helper = reboot_helper(Some(KernelCmdline::new(b"productid=atma root=x\n")));
        assert_eq!(
            helper[helper.len() - 3..],
            [
                PowerStep::BacklightOff,
                PowerStep::InitReboot,
                PowerStep::HalClose
            ]
        );
        assert!(reboot_helper(None).contains(&PowerStep::HalRestart));
    }

    #[test]
    fn safe_policy_remounts_read_only_and_skips_check_when_unmount_fails() {
        let mut stock = Busy { log: Vec::new() };
        let report = execute(&power_off(), Policy::Stock, &mut stock).unwrap();
        assert_eq!(report.failed, [PowerStep::Unmount(USER_DATA_MOUNT)]);
        assert_eq!(stock.log, power_off());

        let mut safe = Busy { log: Vec::new() };
        let report = execute(&power_off(), Policy::Safe, &mut safe).unwrap();
        assert_eq!(
            report.skipped,
            [PowerStep::CheckFilesystem(USER_DATA_DEVICE)]
        );
        assert_eq!(
            safe.log[2..7],
            [
                PowerStep::StopLogPipelines,
                PowerStep::StopLogFollower,
                PowerStep::Sync,
                PowerStep::Unmount(USER_DATA_MOUNT),
                PowerStep::RemountReadOnly(USER_DATA_MOUNT),
            ]
        );
        assert_eq!(safe.log.last(), Some(&PowerStep::HalShutdown));
    }

    #[test]
    fn abort_keeps_the_report_of_steps_that_already_ran() {
        struct Abort;
        impl PowerBackend for Abort {
            type Error = &'static str;
            fn perform(&mut self, step: PowerStep) -> Result<StepStatus, Self::Error> {
                match step {
                    PowerStep::Unmount(_) => Ok(StepStatus::Failure),
                    PowerStep::HalShutdown => Err("no hal"),
                    _ => Ok(StepStatus::Success),
                }
            }
        }
        let abort = execute(&power_off(), Policy::Stock, &mut Abort).unwrap_err();
        assert_eq!(abort.step, PowerStep::HalShutdown);
        assert_eq!(abort.error, "no hal");
        assert_eq!(abort.report.failed, [PowerStep::Unmount(USER_DATA_MOUNT)]);
    }

    #[test]
    fn power_button_polarity_differs_by_product_and_only_the_mini_long_presses() {
        let qc = power_button(Product::QuadCortex, 0);
        assert_eq!((qc.pressed, qc.long_press), (true, LongPress::Keep));
        assert_eq!(
            power_button(Product::QuadCortex, 1).long_press,
            LongPress::Keep
        );
        let mini = power_button(Product::QuadCortexMini, 1);
        assert_eq!((mini.pressed, mini.long_press), (true, LongPress::Start));
        let release = power_button(Product::QuadCortexMini, 0);
        assert_eq!(
            (release.pressed, release.long_press),
            (false, LongPress::Stop)
        );
    }
}

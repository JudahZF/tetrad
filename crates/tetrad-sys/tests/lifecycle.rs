//! Lifecycle tests that start child processes. They live in their own test binary: a child holds copies of
//! every descriptor of the test process until it calls `exec`, which would make the FIFO hangup and EOF checks
//! of the unit tests racy.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use tetrad_hal::node::Node;
use tetrad_sys::ioctl::IoctlNode;
use tetrad_sys::lifecycle::boot::{BootSysError, LinuxBoot, LinuxBootConfig, Spawned};
use tetrad_sys::lifecycle::power::{LinuxPower, NeedsApplication, PowerPrograms};
use tetrad_watchdog::boot::{BootBackend, BootEnv, BootIntent, Launch, Status};
use tetrad_watchdog::power::{Policy, PowerBackend, PowerStep, StepStatus, execute};

/// Serializes the tests that start programs. A fork in one test can hold a copy of a script's write descriptor
/// until that fork execs, so exec of a script another test just wrote can fail with ETXTBSY.
static SPAWN_LOCK: Mutex<()> = Mutex::new(());

fn temp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("tetrad-sys-it-{tag}-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn leak(path: &Path) -> &'static str {
    String::leak(path.to_str().unwrap().to_owned())
}

/// Host-safe programs: nothing is killed, checked or rebooted.
const TEST_PROGRAMS: PowerPrograms = PowerPrograms {
    killall: "true",
    e2fsck: "false",
    reboot_helper: "true",
    backlight_off: "true",
    init_reboot: "false",
};
/// A path that cannot be a mount point, so the mount syscalls only fail.
const NOT_MOUNTED: &str = "/nonexistent/tetrad-sys-power-test";

/// The safe policy over real syscalls and programs: unmount fails, the read-only fallback fails, the check is
/// skipped, and the HAL action is a real ioctl that a pipe refuses. App steps are handed back, not skipped.
#[test]
fn power_steps_report_failures_and_app_steps_are_handed_back() {
    let _spawn = SPAWN_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
    let (reader, _writer) = std::io::pipe().unwrap();
    let soundcard = IoctlNode::from_fd(Node::Soundcard, reader.into());
    let mut power = LinuxPower::with_soundcard(TEST_PROGRAMS, soundcard);
    let steps = [
        PowerStep::StopLogPipelines,
        PowerStep::StopLogFollower,
        PowerStep::Sync,
        PowerStep::Unmount(NOT_MOUNTED),
        PowerStep::Sync,
        PowerStep::CheckFilesystem(NOT_MOUNTED),
        PowerStep::Delay(Duration::from_millis(1)),
        PowerStep::HalShutdown,
        PowerStep::RunRebootHelper,
        PowerStep::InitReboot,
    ];
    let report = execute(&steps, Policy::Safe, &mut power).unwrap();
    assert_eq!(
        report.failed,
        [
            PowerStep::Unmount(NOT_MOUNTED),
            PowerStep::RemountReadOnly(NOT_MOUNTED),
            PowerStep::HalShutdown,
            PowerStep::InitReboot,
        ]
    );
    assert_eq!(report.skipped, [PowerStep::CheckFilesystem(NOT_MOUNTED)]);

    assert_eq!(
        power.perform(PowerStep::MuteHeadphones),
        Err(NeedsApplication(PowerStep::MuteHeadphones))
    );
    assert_eq!(power.perform(PowerStep::HalClose), Ok(StepStatus::Success));
    assert_eq!(
        power.perform(PowerStep::HalRestart),
        Ok(StepStatus::Failure)
    );
    assert_eq!(
        power.perform(PowerStep::CheckFilesystem(NOT_MOUNTED)),
        Ok(StepStatus::Failure)
    );
}

/// Power programs run with stdin, stdout and stderr on `/dev/null`. After `killall tee` their output would
/// otherwise reach a pipe with no reader.
#[test]
fn power_programs_run_with_null_stdio() {
    let _spawn = SPAWN_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
    let dir = temp_dir("null-stdio");
    let helper = script(
        &dir,
        "helper",
        "for fd in 0 1 2; do [ \"$(readlink /proc/$$/fd/$fd)\" = /dev/null ] || exit 3; done\necho \"helper output\"\n",
    );
    let mut power = LinuxPower::new(PowerPrograms {
        reboot_helper: helper,
        ..TEST_PROGRAMS
    });
    assert_eq!(
        power.perform(PowerStep::RunRebootHelper),
        Ok(StepStatus::Success)
    );
    fs::remove_dir_all(&dir).unwrap();
}

fn script(dir: &Path, name: &str, body: &str) -> &'static str {
    let path = dir.join(name);
    fs::write(&path, format!("#!/bin/sh\n{body}")).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    leak(&path)
}

/// The U-Boot gate flashes only on `fw_printenv` exit status exactly 1 (`S80drvinit:66`). Exit 0 carries the
/// output; exit 2 and a program that cannot start are `Failed`. The display read keeps `$? -ne 0` semantics.
#[test]
fn boot_env_status_separates_exit_one_from_other_failures() {
    let _spawn = SPAWN_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
    let dir = temp_dir("fwenv");
    let fake = script(
        &dir,
        "fw_printenv",
        "case \"$1\" in set) echo \"$1=1\";; unset) exit 1;; *) exit 2;; esac\n",
    );
    let mut boot = LinuxBoot::new(LinuxBootConfig {
        fw_printenv: fake,
        ..LinuxBootConfig::stock()
    });
    assert_eq!(
        boot.boot_env_status("set").unwrap(),
        BootEnv::Set(b"set=1\n".to_vec())
    );
    assert_eq!(boot.boot_env_status("unset").unwrap(), BootEnv::NotSet);
    assert_eq!(boot.boot_env_status("broken").unwrap(), BootEnv::Failed);
    assert_eq!(boot.boot_env("unset").unwrap(), None);
    assert_eq!(boot.boot_env("set").unwrap(), Some(b"set=1\n".to_vec()));

    let mut missing = LinuxBoot::new(LinuxBootConfig {
        fw_printenv: "/nonexistent/fw_printenv",
        ..LinuxBootConfig::stock()
    });
    assert_eq!(missing.boot_env_status("unset").unwrap(), BootEnv::Failed);
    fs::remove_dir_all(&dir).unwrap();
}

/// A logged launch runs in its working directory with the launch and exported environment; both output streams
/// reach the log through `tee -a`, which appends.
#[test]
fn logged_launch_pipes_both_streams_through_tee_with_exports() {
    let _spawn = SPAWN_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
    let dir = temp_dir("launch");
    script(
        &dir,
        "emit",
        "echo \"out $TETRAD_EXPORTED $TETRAD_LAUNCH $(basename \"$PWD\")\"\necho err >&2\n",
    );
    let log = dir.join("system.log");
    fs::write(&log, "kept\n").unwrap();

    let mut boot = LinuxBoot::new(LinuxBootConfig {
        allow_bootloader_flash: false,
        ..LinuxBootConfig::stock()
    });
    let export = BootIntent::ExportEnv {
        name: "TETRAD_EXPORTED",
        value: b"bus".to_vec(),
    };
    assert_eq!(boot.perform(export).unwrap(), Status::Success);
    let launch = Launch {
        program: "./emit",
        working_dir: leak(&dir),
        env: &[("TETRAD_LAUNCH", "ui")],
        log: Some(log.clone()),
    };
    assert_eq!(
        boot.perform(BootIntent::Launch(launch)).unwrap(),
        Status::Success
    );
    let mut children = boot.take_children();
    assert_eq!(children.len(), 1);
    let Spawned {
        program,
        child,
        tee,
    } = &mut children[0];
    assert_eq!(*program, dir.join("./emit"));
    assert!(child.wait().unwrap().success());
    assert!(tee.as_mut().unwrap().wait().unwrap().success());
    let text = fs::read_to_string(&log).unwrap();
    let dir_name = dir.file_name().unwrap().to_str().unwrap();
    assert!(text.starts_with("kept\n"), "{text:?}");
    assert!(
        text.contains(&format!("out bus ui {dir_name}\n")),
        "{text:?}"
    );
    assert!(text.contains("err\n"), "{text:?}");
    fs::remove_dir_all(&dir).unwrap();
}

/// A launch that cannot start is recorded and reported as `Failure`. Boot goes on, so the next launch starts.
#[test]
fn failed_launch_is_reported_and_later_launches_still_start() {
    let _spawn = SPAWN_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
    let dir = temp_dir("launch-fail");
    let mut boot = LinuxBoot::new(LinuxBootConfig::stock());

    let missing = Launch {
        program: "./missing",
        working_dir: leak(&dir),
        env: &[],
        log: None,
    };
    assert_eq!(
        boot.perform(BootIntent::Launch(missing)).unwrap(),
        Status::Failure
    );
    let later = Launch {
        program: "true",
        working_dir: leak(&dir),
        env: &[],
        log: None,
    };
    assert_eq!(
        boot.perform(BootIntent::Launch(later)).unwrap(),
        Status::Success
    );

    assert!(matches!(
        boot.take_launch_failures().as_slice(),
        [BootSysError::Launch { .. }]
    ));
    assert_eq!(boot.take_children().len(), 1);
    fs::remove_dir_all(&dir).unwrap();
}

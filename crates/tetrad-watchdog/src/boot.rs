//! `S80drvinit` start sequence (CorOS 4.1.0) as typed intents run by an injected
//! [`BootBackend`]. Order, probes and failure continuation follow the script;
//! line numbers in comments refer to it. Console `echo` output is not modeled.

use std::{collections::BTreeSet, ffi::OsString, path::PathBuf, time::Duration};

use thiserror::Error;

use crate::{
    cmdline::{DrvinitProduct, KernelCmdline, RootPartition},
    splash::SplashCommand,
};

pub const USER_DATA_DEVICE: &str = "/dev/mmcblk0p4";
pub const USER_DATA_MOUNT: &str = "/media/p4";
pub const LOG_DIRECTORY: &str = "/media/p4/logs/system/";
pub const APP_DIRECTORY: &str = "/opt/neuraldsp";
/// Created by `zensplash` (inferred from its strings) to release the boot wait.
pub const SPLASH_START_UI: &str = "/opt/neuraldsp/splashScreen/startUI";
/// Created by `zensplash` when its recovery menu wants a restart.
pub const SPLASH_WANTS_RESTART: &str = "/opt/neuraldsp/splashScreen/wantsToRestart";
pub const MOUNT_ATTEMPTS: u32 = 3;
pub const MOUNT_RETRY_DELAY: Duration = Duration::from_secs(1);
pub const HANDSHAKE_POLL: Duration = Duration::from_millis(5);
/// `ls | wc -l` threshold: one oldest pair is removed at 40 entries or more.
pub const MAX_LOG_ENTRIES: usize = 40;

/// Environment for the UI launch (`S80drvinit:351`).
pub const UI_ENVIRONMENT: [(&str, &str); 7] = [
    ("QWS_MOUSE_PROTO", "tslib"),
    ("TSLIB_FBDEVICE", "/dev/fb0"),
    ("TSLIB_CONSOLEDEVICE", "none"),
    ("TSLIB_CONFFILE", "/etc/ts.conf"),
    ("TSLIB_CALIBFILE", "/etc/pointercal"),
    ("TSLIB_TSDEVICE", "/dev/input/event0"),
    ("QC_SERIAL_NUMBER", "/media/p4/qc_sn"),
];

const JACK_ALIASES: [(&str, &str); 7] = [
    ("/dev/zjack/soundcard", "/dev/zenjack"),
    ("/dev/zjack/soundcard_express1", "/dev/input/event14"),
    ("/dev/zjack/soundcard_express2", "/dev/input/event15"),
    ("/dev/zjack/soundcard_over1", "/dev/zenjack_over1"),
    ("/dev/zjack/soundcard_over2", "/dev/zenjack_over2"),
    ("/dev/zjack/soundcard_input", "/dev/zenjack_input"),
    ("/dev/zjack/soundcard_power_button", "/dev/zenjack_capsense"),
];

const UBOOT_SECTORS: [(u32, &str); 2] = [
    (0x19000, "/opt/neuraldsp/uboot_sector_addr_0x19000"),
    (0x35000, "/opt/neuraldsp/uboot_sector_addr_0x35000"),
];

/// Programs launched at the end of boot. Stock runs `./ZenUI` and
/// `./ZenWatchdog` from [`APP_DIRECTORY`]; a Rust build names its own binaries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BootConfig {
    pub ui_program: &'static str,
    pub watchdog_program: &'static str,
}

impl BootConfig {
    pub const STOCK: Self = Self {
        ui_program: "./ZenUI",
        watchdog_program: "./ZenWatchdog",
    };
}

/// A background child. `log` receives stdout and stderr in append mode, as the
/// stock `2>&1 | tee -a <log>` pipeline does; the pipeline is not required.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Launch {
    pub program: &'static str,
    pub working_dir: &'static str,
    pub env: &'static [(&'static str, &'static str)],
    pub log: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BootIntent {
    Splash(SplashCommand),
    /// `e2fsck -C0 -p -f -v <device>`. Stock ignores the result.
    CheckFilesystem(&'static str),
    Unmount(&'static str),
    MountExt4 {
        device: &'static str,
        target: &'static str,
    },
    Sleep(Duration),
    /// `rm -rf`.
    RemoveTree(PathBuf),
    /// `flash_erase /dev/mtd0 <offset> 1`, `nandwrite -s <offset>`, remove the
    /// image, sync. Firmware maintenance; a backend may refuse it.
    FlashUbootSector {
        offset: u32,
        image: &'static str,
    },
    SetBootEnv {
        name: &'static str,
        value: &'static str,
    },
    /// `echo <value> > <path>` under `/proc/sys`.
    WriteKernelSetting {
        path: &'static str,
        value: &'static str,
    },
    /// `ulimit -c unlimited` in the boot shell; every later child inherits it.
    UnlimitedCoreDumps,
    /// `setserial <device> divisor <n> spd_cust`: MIDI at 31,250 baud.
    CustomUartDivisor {
        device: &'static str,
        divisor: u32,
    },
    /// `mkdir -p`.
    CreateDir(&'static str),
    Symlink {
        link: &'static str,
        target: &'static str,
    },
    /// `touch`.
    CreateFile(PathBuf),
    Sync,
    LoadModule(&'static str),
    /// Foreground helper script; stock ignores its result.
    RunProgram {
        path: &'static str,
        args: Vec<PathBuf>,
    },
    Launch(Launch),
    /// `dmesg > <log>` (truncates).
    WriteKernelLog(PathBuf),
    /// `tail -f /var/log/messages >> <log> &`. It keeps a file open on p4.
    FollowSystemLog(PathBuf),
    WriteFile {
        path: &'static str,
        contents: Vec<u8>,
    },
    AppendToLog {
        log: PathBuf,
        text: String,
    },
    /// `fw_printenv <name> >> <log>`.
    AppendBootEnv {
        name: &'static str,
        log: PathBuf,
    },
    /// `fsck.ext4 -fn <device> | grep non-contiguous >> <log>`.
    AppendFragmentation {
        device: &'static str,
        log: PathBuf,
    },
    /// `zc_gtl` re-encryption of factory capture metadata (lines 298-309).
    TranslateFactoryCaptures,
    /// `export $(dbus-launch)`; later children inherit the session bus.
    StartSessionBus,
    ExportEnv {
        name: &'static str,
        value: Vec<u8>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Probe {
    /// `[ -e ]`
    Exists(&'static str),
    /// `[ -d ]`
    IsDirectory(&'static str),
    /// `[ -f ]`
    IsFile(&'static str),
    /// `mount | grep -q <device>`
    Mounted(&'static str),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Success,
    Failure,
}

/// `fw_printenv <name>` result where the script tests the exact exit status.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BootEnv {
    /// Exit 0, with standard output.
    Set(Vec<u8>),
    /// Exit status exactly 1 (`S80drvinit:66` `[ $? -eq 1 ]`).
    NotSet,
    /// Any other exit status or a signal. Also an unknown status.
    Failed,
}

/// Executes intents and answers observations. An `Err` means the backend could
/// not attempt the action; it aborts the sequence. A command that ran and failed
/// is not an `Err`: [`perform`](Self::perform) returns [`Status::Failure`], and
/// the readers return what the failed stock command gives the script (`ls`:
/// no entries; `cat`: empty text; `fw_printenv`: `None`). Stock waits without a
/// bound for the splash; a backend bounds it by failing [`BootIntent::Sleep`].
pub trait BootBackend {
    type Error;
    fn perform(&mut self, intent: BootIntent) -> Result<Status, Self::Error>;
    fn probe(&mut self, probe: Probe) -> Result<bool, Self::Error>;
    /// `fw_printenv <name>` standard output, or `None` for a nonzero exit.
    fn boot_env(&mut self, name: &'static str) -> Result<Option<Vec<u8>>, Self::Error>;
    /// `fw_printenv <name>` with its exit status, for the U-Boot gate. The
    /// default cannot see the status, so a nonzero exit is [`BootEnv::Failed`]
    /// and the gate never flashes. A real backend overrides it.
    fn boot_env_status(&mut self, name: &'static str) -> Result<BootEnv, Self::Error> {
        Ok(self.boot_env(name)?.map_or(BootEnv::Failed, BootEnv::Set))
    }
    fn read_file(&mut self, path: &'static str) -> Result<Vec<u8>, Self::Error>;
    fn list_dir(&mut self, path: &'static str) -> Result<Vec<OsString>, Self::Error>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogFiles {
    pub system: PathBuf,
    pub watchdog: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BootOutcome {
    /// UI and watchdog were spawned. Spawning is not application readiness.
    Launched,
    /// The splash asked to restart; `2000` was sent and nothing was launched.
    SplashRestart,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BootReport {
    /// Stock boot continues without user data; callers must see it.
    pub user_data_mounted: bool,
    pub logs: LogFiles,
    pub outcome: BootOutcome,
}

#[derive(Debug, PartialEq, Eq, Error)]
pub enum BootError<E> {
    #[error("boot backend failed: {0}")]
    Backend(E),
    #[error("{0}")]
    Logs(LogPlanError),
}

impl<E> From<E> for BootError<E> {
    fn from(error: E) -> Self {
        Self::Backend(error)
    }
}

/// Run the stock start sequence once.
pub fn run<B: BootBackend>(
    config: BootConfig,
    cmdline: KernelCmdline<'_>,
    backend: &mut B,
) -> Result<BootReport, BootError<B::Error>> {
    let splash =
        |backend: &mut B, value| backend.perform(BootIntent::Splash(SplashCommand::new(value)));
    let remove = |path: &str| BootIntent::RemoveTree(PathBuf::from(path));

    // 20-47: check, release and mount user data.
    splash(backend, 2)?;
    backend.perform(BootIntent::CheckFilesystem(USER_DATA_DEVICE))?;
    if backend.probe(Probe::Mounted(USER_DATA_DEVICE))? {
        backend.perform(BootIntent::Unmount(USER_DATA_MOUNT))?;
    }
    let mut user_data_mounted = false;
    for attempt in 1..=MOUNT_ATTEMPTS {
        let mount = BootIntent::MountExt4 {
            device: USER_DATA_DEVICE,
            target: USER_DATA_MOUNT,
        };
        if backend.perform(mount)? == Status::Success {
            user_data_mounted = true;
            break;
        }
        if attempt < MOUNT_ATTEMPTS {
            backend.perform(BootIntent::Sleep(MOUNT_RETRY_DELAY))?;
        }
    }
    for old in ["/media/p4/logs/mmcblk0p1", "/media/p4/logs/mmcblk0p2"] {
        if backend.probe(Probe::IsDirectory(old))? {
            backend.perform(remove(old))?;
        }
    }

    // 57-100: one-time U-Boot sector update on serialized Quad Cortex units.
    splash(backend, 4)?;
    if backend.probe(Probe::Exists("/media/p4/qc_sn"))?
        && cmdline.drvinit_product() == DrvinitProduct::QuadCortex
    {
        if backend.boot_env_status("uboot_version")? == BootEnv::NotSet {
            for (offset, image) in UBOOT_SECTORS {
                if backend.probe(Probe::Exists(image))? {
                    backend.perform(BootIntent::FlashUbootSector { offset, image })?;
                }
            }
            backend.perform(BootIntent::SetBootEnv {
                name: "uboot_version",
                value: "1",
            })?;
        } else {
            for (_, image) in UBOOT_SECTORS {
                backend.perform(remove(image))?;
            }
            backend.perform(BootIntent::Sync)?;
        }
    }

    // 102-132: kernel settings, MIDI UART, display alias.
    splash(backend, 6)?;
    const NEW_VERSION_FLAG: &str = "/media/p4/UpdaterTemp/updater_new_version_available";
    if backend.probe(Probe::Exists(NEW_VERSION_FLAG))? {
        backend.perform(remove(NEW_VERSION_FLAG))?;
    }
    for (path, value) in [
        ("/proc/sys/vm/overcommit_memory", "1"),
        ("/proc/sys/kernel/core_pattern", "/media/p4/coredump"),
    ] {
        backend.perform(BootIntent::WriteKernelSetting { path, value })?;
    }
    backend.perform(BootIntent::UnlimitedCoreDumps)?;
    backend.perform(BootIntent::CustomUartDivisor {
        device: "/dev/ttySC1",
        divisor: 225,
    })?;
    backend.perform(BootIntent::CreateDir("/dev/zendisplay"))?;
    backend.perform(BootIntent::Symlink {
        link: "/dev/zendisplay/fd",
        target: "/dev/zendisplay-dev",
    })?;

    // 132-208: allocate the paired system and watchdog logs.
    splash(backend, 8)?;
    if !backend.probe(Probe::IsDirectory(LOG_DIRECTORY))? {
        backend.perform(BootIntent::CreateDir(LOG_DIRECTORY))?;
    }
    let entries = backend.list_dir(LOG_DIRECTORY)?;
    let plan = plan_logs(&entries, cmdline.root_partition()).map_err(BootError::Logs)?;
    for path in plan.remove {
        backend.perform(BootIntent::RemoveTree(path))?;
    }
    let logs = plan.files;
    backend.perform(BootIntent::CreateFile(logs.system.clone()))?;
    backend.perform(BootIntent::CreateFile(logs.watchdog.clone()))?;
    backend.perform(BootIntent::Sync)?;

    // 210-247: test app, jack driver and aliases, factory helpers.
    splash(backend, 10)?;
    const TEST_MODE: &str = "/opt/neuraldsp/productiontestmode";
    if backend.probe(Probe::Exists(TEST_MODE))? {
        backend.perform(BootIntent::Launch(Launch {
            program: TEST_MODE,
            working_dir: "/",
            env: &[],
            log: None,
        }))?;
    }
    backend.perform(BootIntent::LoadModule("zenjack"))?;
    backend.perform(BootIntent::CreateDir("/dev/zjack"))?;
    for (link, target) in JACK_ALIASES {
        backend.perform(BootIntent::Symlink { link, target })?;
    }
    backend.perform(BootIntent::LoadModule("uio_pdrv_genirq"))?;
    splash(backend, 12)?;
    for path in [
        "/opt/neuraldsp/target_generate_partitions.sh",
        "/opt/neuraldsp/target_push_serialnumber.sh",
    ] {
        if backend.probe(Probe::Exists(path))? {
            backend.perform(BootIntent::RunProgram {
                path,
                args: Vec::new(),
            })?;
        }
    }
    const HW_VERSION_MARKER: &str = "/media/p4/update_hw_version";
    if !backend.probe(Probe::Exists(HW_VERSION_MARKER))? {
        backend.perform(BootIntent::CreateFile(HW_VERSION_MARKER.into()))?;
        backend.perform(BootIntent::Sync)?;
    }

    // 249-286: system log, display type, diagnostics.
    splash(backend, 14)?;
    let system = || logs.system.clone();
    backend.perform(BootIntent::WriteKernelLog(system()))?;
    backend.perform(BootIntent::FollowSystemLog(system()))?;
    // 256: `-ne 0`, so any nonzero exit is "not configured".
    let display = backend.boot_env("QC_DISPLAY")?;
    backend.perform(BootIntent::WriteFile {
        path: "/tmp/qc_display",
        contents: display
            .as_deref()
            .map_or_else(|| b"none\n".to_vec(), cut_second_field),
    })?;
    if display.is_none() {
        backend.perform(BootIntent::AppendToLog {
            log: system(),
            text: "Display type not configured\n".into(),
        })?;
    }
    if !backend.probe(Probe::Mounted(USER_DATA_DEVICE))? {
        backend.perform(BootIntent::AppendToLog {
            log: system(),
            text: "User partition not mounted, system won't behave as expected\n".into(),
        })?;
    }
    backend.perform(BootIntent::AppendBootEnv {
        name: "uboot_version",
        log: system(),
    })?;
    splash(backend, 16)?;
    for device in ["/dev/mmcblk0p1", "/dev/mmcblk0p2", USER_DATA_DEVICE] {
        backend.perform(BootIntent::AppendFragmentation {
            device,
            log: system(),
        })?;
    }
    backend.perform(BootIntent::Sync)?;
    splash(backend, 18)?;
    backend.perform(remove("/etc/updater_status"))?;
    const CHECK_SERIAL: &str = "/opt/neuraldsp/target_check_serialnumber.sh";
    if backend.probe(Probe::Exists(CHECK_SERIAL))? {
        backend.perform(BootIntent::RunProgram {
            path: CHECK_SERIAL,
            args: vec![system()],
        })?;
    }

    // 295-323: one-time capture translation, session bus, cloud endpoint.
    const TRANSLATED: &str = "/opt/neuraldsp/translatedCaptureCombinedFolderEncryption";
    if !backend.probe(Probe::Exists(TRANSLATED))? {
        backend.perform(BootIntent::TranslateFactoryCaptures)?;
        backend.perform(BootIntent::CreateFile(TRANSLATED.into()))?;
        backend.perform(BootIntent::Sync)?;
    }
    backend.perform(BootIntent::StartSessionBus)?;
    const CLOUD_FLAG: &str = "/media/p4/cloud_endpoint_flag";
    if backend.probe(Probe::Exists(CLOUD_FLAG))? {
        let mut value = backend.read_file(CLOUD_FLAG)?;
        while value.last() == Some(&b'\n') {
            value.pop();
        }
        // The stock log line spells the name `CLOUD_ENDOINT`.
        let text = format!("CLOUD_ENDOINT={}\n", String::from_utf8_lossy(&value));
        backend.perform(BootIntent::ExportEnv {
            name: "CLOUD_ENDPOINT",
            value,
        })?;
        backend.perform(BootIntent::AppendToLog {
            log: system(),
            text,
        })?;
    }

    // 333-357: splash handshake, then launch or let the splash restart.
    splash(backend, 20)?;
    backend.perform(BootIntent::Splash(SplashCommand::HAND_OFF))?;
    let restart = loop {
        if backend.probe(Probe::IsFile(SPLASH_START_UI))? {
            break false;
        }
        backend.perform(BootIntent::Sleep(HANDSHAKE_POLL))?;
        if backend.probe(Probe::Exists(SPLASH_WANTS_RESTART))? {
            break true;
        }
    };
    let outcome = if restart {
        backend.perform(BootIntent::Splash(SplashCommand::RESTART_READY))?;
        BootOutcome::SplashRestart
    } else {
        for (program, env, log) in [
            (config.ui_program, &UI_ENVIRONMENT[..], &logs.system),
            (config.watchdog_program, &[][..], &logs.watchdog),
        ] {
            backend.perform(BootIntent::Launch(Launch {
                program,
                working_dir: APP_DIRECTORY,
                env,
                log: Some(log.clone()),
            }))?;
        }
        BootOutcome::Launched
    };
    Ok(BootReport {
        user_data_mounted,
        logs,
        outcome,
    })
}

/// `cut -d '=' -f 2` per line; lines without `=` are kept whole.
fn cut_second_field(output: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(output.len());
    for line in output.split_inclusive(|&b| b == b'\n') {
        let (body, newline) = match line.strip_suffix(b"\n") {
            Some(body) => (body, true),
            None => (line, false),
        };
        let mut fields = body.split(|&b| b == b'=');
        let first = fields.next().unwrap_or_default();
        out.extend_from_slice(fields.next().unwrap_or(first));
        if newline {
            out.push(b'\n');
        }
    }
    out
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogPlan {
    /// The oldest system/watchdog pair when the directory is full.
    pub remove: Vec<PathBuf>,
    pub files: LogFiles,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum LogPlanError {
    /// Stock searches upward forever when entries exist but none is a numbered
    /// p1/p2 system log. Rust reports it instead of hanging boot.
    #[error("log directory has entries but no numbered system log")]
    NoSystemLog,
}

/// Lines 159-199 over a directory listing. Counts every non-hidden entry like
/// `ls | wc -l`, finds the oldest numbered p1/p2 system log, removes one pair
/// at [`MAX_LOG_ENTRIES`], then takes the first free ID above the oldest.
/// A missing root partition gives the stock name `<id>-.log`. Stock searches ID
/// by ID from 0; parsing the names gives the same result in O(n).
pub fn plan_logs(
    entries: &[OsString],
    partition: Option<RootPartition>,
) -> Result<LogPlan, LogPlanError> {
    const P1: &str = "mmcblk0p1";
    const P2: &str = "mmcblk0p2";
    let names: Vec<&[u8]> = entries
        .iter()
        .map(|entry| entry.as_encoded_bytes())
        .filter(|name| !name.starts_with(b"."))
        .collect();
    let system_log = |id: u64, part: &str| format!("{id}-{part}.log");
    let watchdog_log = |id: u64, part: &str| format!("{id}-watchdog-{part}.log");
    let path = |name: String| PathBuf::from(format!("{LOG_DIRECTORY}{name}"));

    let mut remove = Vec::new();
    let id = if names.is_empty() {
        0
    } else {
        let numbered: BTreeSet<u64> = names
            .iter()
            .filter_map(|name| system_log_id(name))
            .collect();
        let oldest = *numbered.first().ok_or(LogPlanError::NoSystemLog)?;
        if names.len() >= MAX_LOG_ENTRIES {
            let p1_name = system_log(oldest, P1);
            let part = if names.contains(&p1_name.as_bytes()) {
                P1
            } else {
                P2
            };
            remove.push(path(system_log(oldest, part)));
            remove.push(path(watchdog_log(oldest, part)));
        }
        // At most `numbered.len()` steps; `oldest` is at most `i64::MAX`.
        (oldest + 1..)
            .find(|id| !numbered.contains(id))
            .unwrap_or(oldest + 1)
    };
    let part = partition
        .map(RootPartition::device_name)
        .unwrap_or_default();
    Ok(LogPlan {
        remove,
        files: LogFiles {
            system: path(system_log(id, &part)),
            watchdog: path(watchdog_log(id, &part)),
        },
    })
}

/// ID of `<id>-mmcblk0p1.log` or `<id>-mmcblk0p2.log` when `<id>` is exactly how
/// ash prints a counter from 0: decimal, no sign, no leading zero, within the
/// 64-bit signed `let` range.
fn system_log_id(name: &[u8]) -> Option<u64> {
    let digits = name
        .strip_suffix(b"-mmcblk0p1.log")
        .or_else(|| name.strip_suffix(b"-mmcblk0p2.log"))?;
    let id: u64 = std::str::from_utf8(digits).ok()?.parse().ok()?;
    (id.to_string().as_bytes() == digits && i64::try_from(id).is_ok()).then_some(id)
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeSet, VecDeque};

    use super::*;
    use crate::splash::BOOT_PROGRESS;

    #[derive(Default)]
    struct Script {
        present: BTreeSet<&'static str>,
        mount_failures: u32,
        mounted: bool,
        handshake: VecDeque<Probe>,
        /// `fw_printenv uboot_version`; `None` uses the trait default.
        uboot_version: Option<BootEnv>,
        intents: Vec<BootIntent>,
    }

    impl BootBackend for Script {
        type Error = &'static str;

        fn perform(&mut self, intent: BootIntent) -> Result<Status, Self::Error> {
            let status = match intent {
                BootIntent::MountExt4 { .. } if self.mount_failures > 0 => {
                    self.mount_failures -= 1;
                    Status::Failure
                }
                BootIntent::MountExt4 { .. } => {
                    self.mounted = true;
                    Status::Success
                }
                _ => Status::Success,
            };
            self.intents.push(intent);
            Ok(status)
        }

        fn probe(&mut self, probe: Probe) -> Result<bool, Self::Error> {
            Ok(match probe {
                Probe::IsFile(SPLASH_START_UI) | Probe::Exists(SPLASH_WANTS_RESTART) => {
                    self.handshake.front() == Some(&probe) && self.handshake.pop_front().is_some()
                }
                Probe::Exists(path) | Probe::IsDirectory(path) | Probe::IsFile(path) => {
                    self.present.contains(path)
                }
                Probe::Mounted(_) => self.mounted,
            })
        }

        fn boot_env(&mut self, name: &'static str) -> Result<Option<Vec<u8>>, Self::Error> {
            Ok((name == "QC_DISPLAY").then(|| b"QC_DISPLAY=lcd=b\n".to_vec()))
        }

        fn boot_env_status(&mut self, name: &'static str) -> Result<BootEnv, Self::Error> {
            match (name, &self.uboot_version) {
                ("uboot_version", Some(env)) => Ok(env.clone()),
                _ => Ok(self.boot_env(name)?.map_or(BootEnv::Failed, BootEnv::Set)),
            }
        }

        fn read_file(&mut self, _: &'static str) -> Result<Vec<u8>, Self::Error> {
            Err("no file reads expected")
        }

        fn list_dir(&mut self, _: &'static str) -> Result<Vec<OsString>, Self::Error> {
            Ok(vec![
                "0-mmcblk0p1.log".into(),
                "0-watchdog-mmcblk0p1.log".into(),
            ])
        }
    }

    fn splash_values(intents: &[BootIntent]) -> Vec<i32> {
        intents
            .iter()
            .filter_map(|intent| match intent {
                BootIntent::Splash(command) => Some(command.value()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn stock_boot_order_retries_mount_waits_for_splash_and_launches_ui_before_watchdog() {
        let mut script = Script {
            mount_failures: 2,
            present: [TRANSLATEDMARK].into(),
            handshake: [Probe::IsFile(SPLASH_START_UI)].into(),
            ..Script::default()
        };
        let cmdline = KernelCmdline::new(b"root=/dev/mmcblk0p2 productid=quadcortex\n");
        let report = run(BootConfig::STOCK, cmdline, &mut script).unwrap();
        let log = |name: &str| PathBuf::from(format!("{LOG_DIRECTORY}{name}"));
        assert_eq!(
            report,
            BootReport {
                user_data_mounted: true,
                logs: LogFiles {
                    system: log("1-mmcblk0p2.log"),
                    watchdog: log("1-watchdog-mmcblk0p2.log"),
                },
                outcome: BootOutcome::Launched,
            }
        );
        let intents = &script.intents;
        assert_eq!(
            splash_values(intents),
            [&BOOT_PROGRESS[..], &[SplashCommand::HAND_OFF.value()]].concat()
        );
        let mount = BootIntent::MountExt4 {
            device: USER_DATA_DEVICE,
            target: USER_DATA_MOUNT,
        };
        let sleep = BootIntent::Sleep(MOUNT_RETRY_DELAY);
        assert_eq!(
            intents[2..7],
            [mount.clone(), sleep.clone(), mount.clone(), sleep, mount]
        );
        assert!(intents.contains(&BootIntent::WriteFile {
            path: "/tmp/qc_display",
            contents: b"lcd\n".to_vec(),
        }));
        assert!(!intents.contains(&BootIntent::TranslateFactoryCaptures));
        let launches: Vec<_> = intents
            .iter()
            .filter_map(|intent| match intent {
                BootIntent::Launch(launch) => Some((launch.program, launch.log.clone())),
                _ => None,
            })
            .collect();
        assert_eq!(
            launches,
            [
                ("./ZenUI", Some(report.logs.system.clone())),
                ("./ZenWatchdog", Some(report.logs.watchdog.clone())),
            ]
        );
        assert_eq!(
            intents
                .iter()
                .position(|i| matches!(i, BootIntent::Splash(c) if *c == SplashCommand::HAND_OFF)),
            Some(intents.len() - 3)
        );
    }

    #[test]
    fn uboot_is_flashed_only_when_fw_printenv_exits_with_exactly_1() {
        let cmdline = KernelCmdline::new(b"productid=quadcortex root=/dev/mmcblk0p1\n");
        let [(offset, image), (_, second)] = UBOOT_SECTORS;
        let run_with = |env: BootEnv| {
            let mut script = Script {
                present: ["/media/p4/qc_sn", image, TRANSLATEDMARK].into(),
                handshake: [Probe::IsFile(SPLASH_START_UI)].into(),
                uboot_version: Some(env),
                ..Script::default()
            };
            run(BootConfig::STOCK, cmdline, &mut script).unwrap();
            script.intents
        };
        let flashed = run_with(BootEnv::NotSet);
        assert!(flashed.contains(&BootIntent::FlashUbootSector { offset, image }));
        assert!(flashed.contains(&BootIntent::SetBootEnv {
            name: "uboot_version",
            value: "1",
        }));
        // Exit 127 (no `fw_printenv`) takes the else branch: remove images only.
        let failed = run_with(BootEnv::Failed);
        assert!(
            !failed
                .iter()
                .any(|i| matches!(i, BootIntent::FlashUbootSector { .. }))
        );
        assert!(failed.contains(&BootIntent::RemoveTree(second.into())));
    }

    const TRANSLATEDMARK: &str = "/opt/neuraldsp/translatedCaptureCombinedFolderEncryption";

    #[test]
    fn splash_restart_request_sends_restart_ready_and_launches_nothing() {
        let mut script = Script {
            present: [TRANSLATEDMARK].into(),
            handshake: [Probe::Exists(SPLASH_WANTS_RESTART)].into(),
            mount_failures: 3,
            ..Script::default()
        };
        let report = run(BootConfig::STOCK, KernelCmdline::new(b""), &mut script).unwrap();
        assert_eq!(report.outcome, BootOutcome::SplashRestart);
        assert!(!report.user_data_mounted);
        assert_eq!(
            report.logs.system,
            PathBuf::from(format!("{LOG_DIRECTORY}1-.log"))
        );
        let tail = &script.intents[script.intents.len() - 3..];
        assert_eq!(
            tail,
            [
                BootIntent::Splash(SplashCommand::HAND_OFF),
                BootIntent::Sleep(HANDSHAKE_POLL),
                BootIntent::Splash(SplashCommand::RESTART_READY),
            ]
        );
        assert!(script.intents.contains(&BootIntent::AppendToLog {
            log: report.logs.system.clone(),
            text: "User partition not mounted, system won't behave as expected\n".into(),
        }));
    }

    #[test]
    fn log_plan_removes_one_oldest_pair_fills_gaps_and_refuses_the_stock_hang() {
        let p1 = RootPartition::new(1);
        let mut entries: Vec<OsString> = (1..=20)
            .flat_map(|id| {
                [
                    format!("{id}-mmcblk0p2.log"),
                    format!("{id}-watchdog-mmcblk0p2.log"),
                ]
            })
            .map(OsString::from)
            .collect();
        entries.push(".hidden".into());
        let plan = plan_logs(&entries, p1).unwrap();
        let path = |name: &str| PathBuf::from(format!("{LOG_DIRECTORY}{name}"));
        assert_eq!(
            plan.remove,
            [path("1-mmcblk0p2.log"), path("1-watchdog-mmcblk0p2.log")]
        );
        assert_eq!(plan.files.system, path("21-mmcblk0p1.log"));
        assert_eq!(plan.files.watchdog, path("21-watchdog-mmcblk0p1.log"));

        // A gap above the oldest is reused, as the stock upward search does.
        let gap: Vec<OsString> = ["0-mmcblk0p1.log", "2-mmcblk0p1.log"]
            .map(OsString::from)
            .into();
        let plan = plan_logs(&gap, p1).unwrap();
        assert!(plan.remove.is_empty());
        assert_eq!(plan.files.system, path("1-mmcblk0p1.log"));

        let empty = plan_logs(&[], p1).unwrap();
        assert_eq!(empty.files.watchdog, path("0-watchdog-mmcblk0p1.log"));
        let orphan = [OsString::from("3-watchdog-mmcblk0p1.log")];
        assert_eq!(plan_logs(&orphan, p1), Err(LogPlanError::NoSystemLog));

        // Large or non-canonical IDs cost O(n), not one step per ID.
        let stray = ["9999999999-x", "007-mmcblk0p1.log", "+5-mmcblk0p1.log"].map(OsString::from);
        assert_eq!(plan_logs(&stray, p1), Err(LogPlanError::NoSystemLog));
        let large = [OsString::from("4000000000-mmcblk0p1.log")];
        let plan = plan_logs(&large, p1).unwrap();
        assert_eq!(plan.files.system, path("4000000001-mmcblk0p1.log"));
    }
}

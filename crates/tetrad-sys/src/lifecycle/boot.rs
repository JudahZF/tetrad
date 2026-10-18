//! Linux backend for `tetrad_watchdog::boot::run`, the `S80drvinit` start sequence. Line numbers refer to that
//! script. Each shell line becomes a syscall, a file operation or one program run without a shell; the
//! programs are the stock BusyBox, e2fsprogs, mtd-utils and U-Boot tools found through `PATH`.

use std::ffi::{OsStr, OsString};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::num::NonZeroU32;
use std::os::fd::OwnedFd;
use std::os::unix::ffi::OsStringExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, SystemTime};

use tetrad_watchdog::boot::{BootBackend, BootEnv, BootIntent, Launch, Probe, Status};
use tetrad_watchdog::splash::SplashQueue;
use thiserror::Error;

use super::splash::PosixSplashQueue;
use super::{E2FSCK_ARGS, mount_ext4, run, sync, unmount};
use crate::serial;

const MTD0: &str = "/dev/mtd0";
/// Lines 298-309: factory capture metadata re-encryption.
const FACTORY_CAPTURES: &str = "/opt/neuraldsp/FactoryCaptures";
const SELF_TEST_CAPTURES: [&str; 2] = [
    "/opt/neuraldsp/sharc_binaries/eltron_self_test.json",
    "/opt/neuraldsp/sharc_binaries/eltron_self_test.cns",
];
const ZC_GTL: &str = "/opt/neuraldsp/zct/zc_gtl";

#[derive(Debug, Clone)]
pub struct LinuxBootConfig {
    /// Progress goes straight to the queue (owned; stock runs `/opt/neuraldsp/splashqueue N`).
    pub splash: PosixSplashQueue,
    /// Text searched by `Probe::Mounted` (`mount | grep -q <device>`).
    pub mount_table: &'static str,
    /// Total requested `BootIntent::Sleep` time. `None` is stock: the splash wait has no bound.
    pub sleep_budget: Option<Duration>,
    /// `BootIntent::FlashUbootSector` rewrites U-Boot sectors on `/dev/mtd0` (lines 70-84). `false` refuses it
    /// with an error, which stops boot before `fw_setenv uboot_version 1`.
    pub allow_bootloader_flash: bool,
    /// U-Boot environment reader: exit 0 with `NAME=value`, exit 1 when unset or unreadable.
    pub fw_printenv: &'static str,
}

impl LinuxBootConfig {
    /// Stock behavior, flash included. The 4.1.0 rootfs ships no `uboot_sector_addr_*` image, so the flash
    /// intent is not produced there.
    pub fn stock() -> Self {
        Self {
            splash: PosixSplashQueue::stock(),
            mount_table: "/proc/mounts",
            sleep_budget: None,
            allow_bootloader_flash: true,
            fw_printenv: "fw_printenv",
        }
    }
}

#[derive(Debug, Error)]
pub enum BootSysError {
    #[error("boot sleep budget of {0:?} is spent")]
    SleepBudget(Duration),
    #[error("U-Boot sector flash at {offset:#x} refused by configuration")]
    BootloaderFlashRefused { offset: u32 },
    #[error("cannot launch {}: {source}", program.display())]
    Launch { program: PathBuf, source: io::Error },
    #[error("{}: {source}", path.display())]
    Io { path: PathBuf, source: io::Error },
}

/// A background child. `tee` is the `tee -a <log>` process that holds the log file, as in stock, so the
/// stock power-off `killall tee` still releases `/media/p4`.
#[derive(Debug)]
pub struct Spawned {
    pub program: PathBuf,
    pub child: Child,
    pub tee: Option<Child>,
}

/// Executes `boot::run` intents. Exported variables (`dbus-launch`, `CLOUD_ENDPOINT`) go to every later child.
#[derive(Debug)]
pub struct LinuxBoot {
    config: LinuxBootConfig,
    exported: Vec<(OsString, OsString)>,
    slept: Duration,
    children: Vec<Spawned>,
    launch_failures: Vec<BootSysError>,
}

impl LinuxBoot {
    pub fn new(config: LinuxBootConfig) -> Self {
        Self {
            config,
            exported: Vec::new(),
            slept: Duration::ZERO,
            children: Vec::new(),
            launch_failures: Vec::new(),
        }
    }

    /// Background children in start order: test mode, `tail -f`, UI, watchdog. Their PIDs are the spawned
    /// identities a supervisor can watch. A boot process may exit without waiting; init reaps them.
    pub fn take_children(&mut self) -> Vec<Spawned> {
        std::mem::take(&mut self.children)
    }

    /// Launches that could not start, in order. Boot went on without them, as stock does past a failed `&`.
    pub fn take_launch_failures(&mut self) -> Vec<BootSysError> {
        std::mem::take(&mut self.launch_failures)
    }

    fn command(&self, program: impl AsRef<OsStr>) -> Command {
        let mut command = Command::new(program);
        command.envs(self.exported.iter().map(|(name, value)| (name, value)));
        command
    }

    fn export(&mut self, name: OsString, value: OsString) {
        self.exported.retain(|(existing, _)| *existing != name);
        self.exported.push((name, value));
    }

    fn sleep(&mut self, duration: Duration) -> Result<(), BootSysError> {
        let slept = self.slept.saturating_add(duration);
        if let Some(budget) = self.config.sleep_budget
            && slept > budget
        {
            return Err(BootSysError::SleepBudget(budget));
        }
        self.slept = slept;
        std::thread::sleep(duration);
        Ok(())
    }

    /// `cd <dir> && VAR=… <program> 2>&1 | tee -a <log> &`. Stdin is `/dev/null`, as for an `&` list in a
    /// non-interactive shell. A program with a `/` resolves against `working_dir`; a bare name uses `PATH`.
    fn launch(&mut self, launch: Launch) -> Result<(), BootSysError> {
        let program = if launch.program.contains('/') {
            Path::new(launch.working_dir).join(launch.program)
        } else {
            PathBuf::from(launch.program)
        };
        let failed = |source| BootSysError::Launch {
            program: program.clone(),
            source,
        };
        let mut command = self.command(&program);
        command
            .current_dir(launch.working_dir)
            .envs(launch.env.iter().copied())
            .stdin(Stdio::null());
        let tee = match &launch.log {
            None => None,
            Some(log) => {
                let mut tee = self
                    .command("tee")
                    .arg("-a")
                    .arg(log)
                    .stdin(Stdio::piped())
                    .spawn()
                    .map_err(failed)?;
                let pipe = OwnedFd::from(tee.stdin.take().expect("tee stdin is piped"));
                command
                    .stdout(pipe.try_clone().map_err(failed)?)
                    .stderr(pipe);
                Some(tee)
            }
        };
        // On failure `command` drops its pipe ends, so `tee` reads EOF and exits.
        let child = command.spawn().map_err(failed)?;
        self.children.push(Spawned {
            program,
            child,
            tee,
        });
        Ok(())
    }

    /// `fw_printenv <name>`; `None` when it cannot start (the shell would report 127).
    fn fw_printenv(&self, name: &str) -> Option<Output> {
        self.command(self.config.fw_printenv)
            .arg(name)
            .stderr(Stdio::inherit())
            .output()
            .ok()
    }

    /// `tail -f /var/log/messages >> <log> &` (line 253). It keeps a file open on `/media/p4`.
    fn follow_system_log(&mut self, log: &Path) -> io::Result<()> {
        let child = self
            .command("tail")
            .args(["-f", "/var/log/messages"])
            .stdin(Stdio::null())
            .stdout(append(log)?)
            .spawn()?;
        self.children.push(Spawned {
            program: "tail".into(),
            child,
            tee: None,
        });
        Ok(())
    }

    /// Lines 70-84 for one sector: `flash_erase`, `nandwrite -s`, `rm -rf <image>`, `sync`; every step runs.
    fn flash_uboot_sector(&self, offset: u32, image: &str) -> Result<bool, BootSysError> {
        if !self.config.allow_bootloader_flash {
            return Err(BootSysError::BootloaderFlashRefused { offset });
        }
        let offset = format!("{offset:#x}");
        let erased = run(self.command("flash_erase").args([MTD0, &offset, "1"]));
        let written = run(self.command("nandwrite").args(["-s", &offset, MTD0, image]));
        let removed = remove_tree(Path::new(image)).is_ok();
        sync();
        Ok(erased && written && removed)
    }

    /// `fsck.ext4 -fn <device> | grep non-contiguous >> <log>`; success when a line matched, like `grep`.
    fn append_fragmentation(&self, device: &str, log: &Path) -> io::Result<bool> {
        let output = self
            .command("fsck.ext4")
            .args(["-fn", device])
            .stderr(Stdio::inherit())
            .output()?;
        let mut file = append(log)?;
        let mut matched = false;
        for line in output.stdout.split_inclusive(|&b| b == b'\n') {
            if line.windows(14).any(|w| w == b"non-contiguous") {
                file.write_all(line)?;
                matched = true;
            }
        }
        Ok(matched)
    }

    /// Lines 298-309: `zc_gtl <f> <f> 0 quiet` for every `*.cns`, then every `*.json` under the factory
    /// captures (directory order, like `find`), then the two self-test files. Success when every run succeeded.
    fn translate_factory_captures(&self) -> bool {
        let mut files = Vec::new();
        for suffix in [".cns", ".json"] {
            find_suffix(Path::new(FACTORY_CAPTURES), suffix, &mut files);
        }
        files.extend(SELF_TEST_CAPTURES.map(PathBuf::from));
        files.iter().fold(true, |ok, file| {
            let translated = run(self
                .command(ZC_GTL)
                .env("QC_SERIAL_NUMBER", "/media/p4/qc_sn")
                .arg(file)
                .arg(file)
                .args(["0", "quiet"]));
            ok && translated
        })
    }

    /// `export $(dbus-launch)` (line 318): every whitespace-separated `NAME=value` word is exported.
    fn start_session_bus(&mut self) -> bool {
        let output = match self
            .command("dbus-launch")
            .stderr(Stdio::inherit())
            .output()
        {
            Ok(output) if output.status.success() => output.stdout,
            _ => return false,
        };
        for word in output.split(u8::is_ascii_whitespace) {
            if let Some(at) = word.iter().position(|&b| b == b'=')
                && at > 0
            {
                self.export(
                    OsString::from_vec(word[..at].to_vec()),
                    OsString::from_vec(word[at + 1..].to_vec()),
                );
            }
        }
        true
    }
}

impl BootBackend for LinuxBoot {
    type Error = BootSysError;

    fn perform(&mut self, intent: BootIntent) -> Result<Status, BootSysError> {
        let success = match intent {
            // Owned: a full queue fails after the configured timeout; stock blocks.
            BootIntent::Splash(command) => self.config.splash.send(command).is_ok(),
            BootIntent::CheckFilesystem(device) => {
                run(self.command("e2fsck").args(E2FSCK_ARGS).arg(device))
            }
            BootIntent::Unmount(target) => unmount(target).is_ok(),
            BootIntent::MountExt4 { device, target } => mount_ext4(device, target).is_ok(),
            BootIntent::Sleep(duration) => {
                self.sleep(duration)?;
                true
            }
            BootIntent::RemoveTree(path) => remove_tree(&path).is_ok(),
            BootIntent::FlashUbootSector { offset, image } => {
                self.flash_uboot_sector(offset, image)?
            }
            BootIntent::SetBootEnv { name, value } => {
                run(self.command("fw_setenv").args([name, value]))
            }
            BootIntent::WriteKernelSetting { path, value } => {
                fs::write(path, format!("{value}\n")).is_ok()
            }
            // `ulimit -c unlimited` sets the soft and hard limit of the boot shell; children inherit them.
            BootIntent::UnlimitedCoreDumps => {
                let unlimited = libc::rlimit {
                    rlim_cur: libc::RLIM_INFINITY,
                    rlim_max: libc::RLIM_INFINITY,
                };
                // SAFETY: setrlimit only reads the live, initialized `unlimited`.
                unsafe { libc::setrlimit(libc::RLIMIT_CORE, &unlimited) == 0 }
            }
            // `setserial <tty> divisor <n> spd_cust`, in process: a non-blocking open and the divisor ioctls only.
            BootIntent::CustomUartDivisor { device, divisor } => NonZeroU32::new(divisor)
                .is_some_and(|divisor| {
                    serial::set_custom_divisor(Path::new(device), divisor).is_ok()
                }),
            BootIntent::CreateDir(path) => fs::create_dir_all(path).is_ok(),
            // `ln -s <target> <link>` fails when `link` exists.
            BootIntent::Symlink { link, target } => {
                std::os::unix::fs::symlink(target, link).is_ok()
            }
            BootIntent::CreateFile(path) => touch(&path).is_ok(),
            BootIntent::Sync => {
                sync();
                true
            }
            BootIntent::LoadModule(module) => run(self.command("modprobe").arg(module)),
            BootIntent::RunProgram { path, args } => run(self.command(path).args(&args)),
            // Stock `&` never fails the script: a launch that cannot start is recorded, and boot goes on.
            BootIntent::Launch(launch) => match self.launch(launch) {
                Ok(()) => true,
                Err(error) => {
                    self.launch_failures.push(error);
                    false
                }
            },
            // `dmesg > <log>`: the shell truncates the file even when dmesg fails.
            BootIntent::WriteKernelLog(log) => {
                File::create(&log).is_ok_and(|file| run(self.command("dmesg").stdout(file)))
            }
            BootIntent::FollowSystemLog(log) => self.follow_system_log(&log).is_ok(),
            BootIntent::WriteFile { path, contents } => fs::write(path, contents).is_ok(),
            BootIntent::AppendToLog { log, text } => append(&log)
                .and_then(|mut file| file.write_all(text.as_bytes()))
                .is_ok(),
            BootIntent::AppendBootEnv { name, log } => append(&log).is_ok_and(|file| {
                run(self.command(self.config.fw_printenv).arg(name).stdout(file))
            }),
            BootIntent::AppendFragmentation { device, log } => {
                self.append_fragmentation(device, &log).unwrap_or(false)
            }
            BootIntent::TranslateFactoryCaptures => self.translate_factory_captures(),
            BootIntent::StartSessionBus => self.start_session_bus(),
            BootIntent::ExportEnv { name, value } => {
                self.export(name.into(), OsString::from_vec(value));
                true
            }
        };
        Ok(if success {
            Status::Success
        } else {
            Status::Failure
        })
    }

    /// `[ -e ]`, `[ -d ]` and `[ -f ]` follow symlinks; any `stat` failure is false.
    fn probe(&mut self, probe: Probe) -> Result<bool, BootSysError> {
        Ok(match probe {
            Probe::Exists(path) => fs::metadata(path).is_ok(),
            Probe::IsDirectory(path) => fs::metadata(path).is_ok_and(|meta| meta.is_dir()),
            Probe::IsFile(path) => fs::metadata(path).is_ok_and(|meta| meta.is_file()),
            // `mount | grep -q <device>` is a substring test over the mount table.
            Probe::Mounted(device) => {
                let table =
                    fs::read(self.config.mount_table).map_err(|source| BootSysError::Io {
                        path: self.config.mount_table.into(),
                        source,
                    })?;
                let device = device.as_bytes();
                !device.is_empty() && table.windows(device.len()).any(|w| w == device)
            }
        })
    }

    /// Exit 0 → `Some(stdout)`; any other result → `None`, as the `$? -ne 0` test (line 256) reads it.
    fn boot_env(&mut self, name: &'static str) -> Result<Option<Vec<u8>>, BootSysError> {
        Ok(self
            .fw_printenv(name)
            .and_then(|output| output.status.success().then_some(output.stdout)))
    }

    /// The U-Boot gate (line 66, `[ $? -eq 1 ]`): only exit status exactly 1 is `NotSet`. A signal, another
    /// status or a program that cannot start (shell 127) is `Failed`, so boot never flashes on it.
    fn boot_env_status(&mut self, name: &'static str) -> Result<BootEnv, BootSysError> {
        Ok(match self.fw_printenv(name) {
            Some(output) if output.status.success() => BootEnv::Set(output.stdout),
            Some(output) if output.status.code() == Some(1) => BootEnv::NotSet,
            _ => BootEnv::Failed,
        })
    }

    fn read_file(&mut self, path: &'static str) -> Result<Vec<u8>, BootSysError> {
        fs::read(path).map_err(|source| BootSysError::Io {
            path: path.into(),
            source,
        })
    }

    fn list_dir(&mut self, path: &'static str) -> Result<Vec<OsString>, BootSysError> {
        let io_error = |source| BootSysError::Io {
            path: path.into(),
            source,
        };
        fs::read_dir(path)
            .map_err(io_error)?
            .map(|entry| entry.map(|entry| entry.file_name()).map_err(io_error))
            .collect()
    }
}

/// `>> <path>`: create if missing, append.
fn append(path: &Path) -> io::Result<File> {
    OpenOptions::new().create(true).append(true).open(path)
}

/// `touch`: create without truncating, then set the modification time to now.
fn touch(path: &Path) -> io::Result<()> {
    append(path)?.set_modified(SystemTime::now())
}

/// `rm -rf`: a missing path is success; a symlink is removed, never followed.
fn remove_tree(path: &Path) -> io::Result<()> {
    let removed = match fs::symlink_metadata(path) {
        Ok(meta) if meta.is_dir() => fs::remove_dir_all(path),
        Ok(_) => fs::remove_file(path),
        Err(error) => Err(error),
    };
    match removed {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        other => other,
    }
}

/// `find <dir> -name '*<suffix>'` without following symlinks; unreadable directories are skipped.
fn find_suffix(dir: &Path, suffix: &str, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if entry
            .file_name()
            .as_encoded_bytes()
            .ends_with(suffix.as_bytes())
        {
            out.push(path.clone());
        }
        if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            find_suffix(&path, suffix, out);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("tetrad-sys-boot-{tag}-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn leak(path: &Path) -> &'static str {
        String::leak(path.to_str().unwrap().to_owned())
    }

    fn boot(mount_table: &'static str, sleep_budget: Option<Duration>) -> LinuxBoot {
        LinuxBoot::new(LinuxBootConfig {
            mount_table,
            sleep_budget,
            allow_bootloader_flash: false,
            ..LinuxBootConfig::stock()
        })
    }

    fn status(boot: &mut LinuxBoot, intent: BootIntent) -> Status {
        boot.perform(intent).unwrap()
    }

    /// File intents follow shell semantics (`ln -s` refuses an existing link, `rm -rf` removes a link but not
    /// its target and accepts a missing path, `>>` appends, `touch` keeps contents), probes follow `test`, and
    /// the mount probe is a substring search. The sleep budget and the flash refusal are hard errors.
    #[test]
    fn file_intents_and_probes_follow_shell_semantics_and_bounds_are_errors() {
        let dir = temp_dir("files");
        let table = dir.join("mounts");
        fs::write(&table, "/dev/mmcblk0p4 /media/p4 ext4 rw 0 0\n").unwrap();
        let mut boot = boot(leak(&table), Some(Duration::from_millis(3)));
        let (nested, link) = (leak(&dir.join("a/b")), leak(&dir.join("a/link")));
        let log = dir.join("a/log");

        assert_eq!(
            status(&mut boot, BootIntent::CreateDir(nested)),
            Status::Success
        );
        let symlink = || BootIntent::Symlink {
            link,
            target: nested,
        };
        assert_eq!(status(&mut boot, symlink()), Status::Success);
        assert_eq!(status(&mut boot, symlink()), Status::Failure);
        for text in ["one\n", "two\n"] {
            let log = log.clone();
            let text = text.into();
            assert_eq!(
                status(&mut boot, BootIntent::AppendToLog { log, text }),
                Status::Success
            );
        }
        assert_eq!(
            status(&mut boot, BootIntent::CreateFile(log.clone())),
            Status::Success
        );
        assert_eq!(fs::read_to_string(&log).unwrap(), "one\ntwo\n");

        assert!(boot.probe(Probe::IsDirectory(link)).unwrap());
        assert!(!boot.probe(Probe::IsFile(link)).unwrap());
        assert!(boot.probe(Probe::IsFile(leak(&log))).unwrap());
        assert!(boot.probe(Probe::Mounted("/dev/mmcblk0p4")).unwrap());
        assert!(!boot.probe(Probe::Mounted("/dev/mmcblk0p1")).unwrap());
        let mut names = boot.list_dir(leak(&dir.join("a"))).unwrap();
        names.sort();
        assert_eq!(names, ["b", "link", "log"]);

        for _ in 0..2 {
            assert_eq!(
                status(&mut boot, BootIntent::RemoveTree(link.into())),
                Status::Success
            );
        }
        assert!(Path::new(nested).is_dir());

        let sleep = BootIntent::Sleep(Duration::from_millis(2));
        assert_eq!(status(&mut boot, sleep.clone()), Status::Success);
        assert!(matches!(
            boot.perform(sleep),
            Err(BootSysError::SleepBudget(_))
        ));
        let flash = BootIntent::FlashUbootSector {
            offset: 0x19000,
            image: "/nonexistent/uboot",
        };
        assert!(matches!(
            boot.perform(flash),
            Err(BootSysError::BootloaderFlashRefused { offset: 0x19000 })
        ));
        fs::remove_dir_all(&dir).unwrap();
    }
}

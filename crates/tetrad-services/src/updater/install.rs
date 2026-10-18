//! A/B install through the Tetrad installer [`INSTALLER`] (`scripts/launcher/tetrad-update.sh`).
//!
//! The installer runs the running root's stock `/opt/neuraldsp/sysupdate_run.sh` with one added
//! step. The stock script picks the inactive root (p1↔p2), formats it, copies the rootfs,
//! translates per-device encryption with `zc_gtl`, copies DTBs and kernel, flips the p3 boot marker
//! and writes `UPDATER_*` tokens to `/etc/updater_status`. The added step runs before the marker
//! flip: it puts the Tetrad boot hook and the SSH keys on the new root (`docs/updates.md`). This
//! executor starts the installer, polls the status file and maps the exit code. It never edits
//! partitions itself.
//!
//! Tetrad never runs the stock script directly: the new root would boot without the hook.

use std::path::Path;

use thiserror::Error;

use super::paths;
use crate::exec::{
    ChildProcess, Command, CommandError, CommandRunner, ExitStatus, FileError, Files,
};

/// The Tetrad installer on p4, run with `/bin/sh` in [`paths::SCRIPT_DIR`] as stock runs
/// `sysupdate_run.sh`.
pub const INSTALLER: &str = "/media/p4/tetrad/launcher/tetrad-update.sh";
/// Stock poll period of the status worker (VA 0x004e8824).
pub const STATUS_POLL_MS: u64 = 1000;
/// `UPDATER_STEP_1..=15`; the stock bar is `n * 100 / 14`, so 14 and 15 both read 100.
pub const LAST_STEP: u8 = 15;
/// ZenUI reports a process that did not start as exit 99 (VA 0x004e6830).
pub const START_FAILED_CODE: i32 = 99;
/// Largest status file that is read. A valid token is at most 15 bytes.
pub const STATUS_MAX_BYTES: usize = 64;

/// One installer milestone, `UPDATER_STEP_1` ..= `UPDATER_STEP_15`. The script publishes it before
/// the phase runs, so it does not prove that the phase worked.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Step(u8);

impl Step {
    pub fn new(n: u8) -> Option<Self> {
        (1..=LAST_STEP).contains(&n).then_some(Self(n))
    }

    pub fn get(self) -> u8 {
        self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusToken {
    Start,
    Step(Step),
    Retry,
    Failure,
    Complete,
}

impl StatusToken {
    /// Stock install bar (VA 0x004e8824): START 0, step `n * 100 / 14` capped at 100.
    pub fn percent(self) -> Option<u8> {
        match self {
            Self::Start => Some(0),
            Self::Step(step) => Some((u16::from(step.0) * 100 / 14).min(100) as u8),
            _ => None,
        }
    }
}

/// Strict read: exactly one token and an optional `\n`. Empty (the `echo >` truncation), oversized
/// and other content is `None`. Stock matches substrings; this does not.
pub fn parse_status(raw: &[u8]) -> Option<StatusToken> {
    if raw.len() > STATUS_MAX_BYTES {
        return None;
    }
    let body = raw.strip_suffix(b"\n").unwrap_or(raw);
    match body {
        b"UPDATER_START" => Some(StatusToken::Start),
        b"UPDATER_RETRY" => Some(StatusToken::Retry),
        b"UPDATER_FAILURE" => Some(StatusToken::Failure),
        b"UPDATER_COMPLETE" => Some(StatusToken::Complete),
        _ => {
            let digits = body.strip_prefix(b"UPDATER_STEP_")?;
            if digits.first() == Some(&b'0') || !digits.iter().all(u8::is_ascii_digit) {
                return None;
            }
            std::str::from_utf8(digits)
                .ok()?
                .parse()
                .ok()
                .and_then(Step::new)
                .map(StatusToken::Step)
        }
    }
}

/// Every failure exit of `sysupdate_run.sh` (exit lines 50–758 and 672), and the two of
/// [`INSTALLER`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallerFailure {
    /// Exit 218 (Tetrad): the hook or key step before the marker flip failed. The stock
    /// failure cleanup ran; the marker did not change, so the running root boots again.
    HookReapply,
    /// Exit 220 (Tetrad): refused before the stock script started; nothing was written. For
    /// example the stock script is not the pinned one, or the package fails `gzip -t`.
    Refused,
    /// Exit 1: `zc_gtl` per-device translation failed.
    DataTranslation,
    BaseMountInUse,
    FlagMountDirCreate,
    FlagPartitionMounted,
    FlagPartitionMount,
    UnrecognizedRoot,
    NewRootDirCreate,
    UpdateDirCreate,
    RootfsDirCreate,
    InactiveRootMounted,
    Gunzip,
    TarExtract,
    RootfsImageMount,
    FormatInactive,
    InactiveRootMount,
    RootfsCopy,
    DtbCopy,
    KernelCopy,
}

impl InstallerFailure {
    const TABLE: [(i32, Self); 20] = [
        (1, Self::DataTranslation),
        (201, Self::BaseMountInUse),
        (202, Self::FlagMountDirCreate),
        (203, Self::FlagPartitionMounted),
        (204, Self::FlagPartitionMount),
        (205, Self::UnrecognizedRoot),
        (206, Self::NewRootDirCreate),
        (207, Self::UpdateDirCreate),
        (208, Self::RootfsDirCreate),
        (209, Self::InactiveRootMounted),
        (210, Self::Gunzip),
        (211, Self::TarExtract),
        (212, Self::RootfsImageMount),
        (213, Self::FormatInactive),
        (214, Self::InactiveRootMount),
        (215, Self::RootfsCopy),
        (216, Self::DtbCopy),
        (217, Self::KernelCopy),
        (218, Self::HookReapply),
        (220, Self::Refused),
    ];

    pub fn from_code(code: i32) -> Option<Self> {
        Self::TABLE
            .iter()
            .find(|(c, _)| *c == code)
            .map(|(_, f)| *f)
    }

    pub fn code(self) -> i32 {
        Self::TABLE
            .iter()
            .find(|(_, f)| *f == self)
            .map_or(0, |(c, _)| *c)
    }

    /// The status the script writes just before this exit.
    fn published_on_exit(self) -> PublishedOnExit {
        match self {
            Self::BaseMountInUse | Self::FlagPartitionMounted => PublishedOnExit::Retry,
            Self::FlagMountDirCreate => PublishedOnExit::RetryOrFailure,
            Self::Refused => PublishedOnExit::Nothing,
            _ => PublishedOnExit::Failure,
        }
    }

    /// Last step the script published before this exit. `None` means only START.
    fn last_step_before(self) -> Option<Step> {
        let n = match self {
            Self::Refused
            | Self::BaseMountInUse
            | Self::FlagMountDirCreate
            | Self::FlagPartitionMounted
            | Self::FlagPartitionMount => return None,
            Self::UnrecognizedRoot => 1,
            Self::NewRootDirCreate | Self::UpdateDirCreate | Self::RootfsDirCreate => 2,
            Self::InactiveRootMounted => 3,
            Self::Gunzip => 4,
            Self::TarExtract => 5,
            Self::RootfsImageMount => 6,
            Self::FormatInactive => 7,
            Self::InactiveRootMount => 8,
            Self::RootfsCopy => 9,
            Self::DataTranslation => 10,
            Self::DtbCopy => 11,
            Self::KernelCopy | Self::HookReapply => 12,
        };
        Step::new(n)
    }
}

/// Status the script writes just before a failure exit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PublishedOnExit {
    Retry,
    /// Exit 202: RETRY if the directory exists after a failed `mkdir`, else FAILURE.
    RetryOrFailure,
    Failure,
    /// Exit 220: the stock script never started, so no token.
    Nothing,
}

/// ZenUI error text, `std::map<int, QString>` built at VA 0x00098860: 24 messages shared by
/// cloud (1, 3–5), download (101–107) and installer (201–217) codes. Display parity only: exit 1
/// (translation) shows "Server authentication failure", and 210/211 predate the current script.
pub fn stock_error_message(code: i32) -> Option<&'static str> {
    Some(match code {
        1 => "Server authentication failure",
        3 => "Server communication failure",
        4 => "Server busy, try again later",
        5 => "Wrong update mode",
        101 => "Partition 3 not found",
        102 | 203 => "Partition 3 in use",
        103 | 202 => "Unable to create mount point for partition 3",
        104 | 204 => "Unable to mount partition 3",
        105 | 210 => "Update package integrity check failure",
        106 => "Unabe to umount partition 3",
        107 => "Unable to remove mount point for partition 3",
        201 => "Base mount point in use",
        205 => "System partition not 1 or 2",
        206 => "Unable to create mount point for new system partition",
        207 => "Unable to create mount point for update package",
        208 => "Unable to create mount point for rootfs",
        209 => "New System partition in use",
        211 => "Update package uncompress failure",
        212 => "Unable to mount rootfs",
        213 => "Unable to format new system partition",
        214 => "Unable to mount new system partition",
        215 => "Unable to copy rootfs files onto system partition",
        216 => "Unable to copy dtb file onto system partition",
        217 => "Unable to copy kernel file onto system partition",
        _ => return None,
    })
}

/// Stock UI text for a failed run: `"%1 (error code %2)"` (VA 0x004e5cbc).
pub fn stock_failure_text(code: i32) -> String {
    let message = stock_error_message(code).unwrap_or("Unknown error");
    format!("{message} (error code {code})")
}

/// The text for a failed run of [`INSTALLER`]: its own exits 218 and 220, else the stock
/// text of [`stock_failure_text`].
pub fn failure_text(code: i32) -> String {
    let message = match InstallerFailure::from_code(code) {
        Some(InstallerFailure::HookReapply) => {
            "Tetrad could not keep its boot hook on the new CorOS. The update did not install, \
             and the current CorOS stays"
        }
        Some(InstallerFailure::Refused) => {
            "The Tetrad installer refused the update before it wrote anything. The system log \
             gives the reason"
        }
        _ => return stock_failure_text(code),
    };
    format!("{message} (error code {code})")
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum InstallError {
    #[error("no update package at {0}")]
    NoPackage(&'static str),
    /// Without [`INSTALLER`] Tetrad does not install: the stock script alone would drop the
    /// boot hook.
    #[error("the Tetrad installer {0} is missing; upload the Tetrad launcher files again")]
    NoInstaller(&'static str),
    #[error(transparent)]
    File(#[from] FileError),
    #[error(transparent)]
    Command(#[from] CommandError),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstallOutcome {
    /// Exit 0 after `UPDATER_COMPLETE`. The p3 marker now selects the new root; a restart boots
    /// it. This is the stock claim only: the final fsck result is unchecked by the script.
    Installed,
    Failed {
        /// `None` when the shell died from a signal.
        code: Option<i32>,
        failure: Option<InstallerFailure>,
        /// Last token read from the status file.
        last: Option<StatusToken>,
        /// False when `last` or the last step does not fit what the script writes before this exit.
        status_consistent: bool,
        /// Stock UI text.
        text: String,
    },
}

impl InstallOutcome {
    /// Only a published RETRY advises a second run. Exit 202 alone does not: it may publish FAILURE.
    pub fn retry_advised(&self) -> bool {
        matches!(
            self,
            Self::Failed {
                last: Some(StatusToken::Retry),
                ..
            }
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstallPoll {
    Running {
        last: Option<StatusToken>,
        percent: u8,
    },
    Finished(InstallOutcome),
}

/// One running installer. Poll it every [`STATUS_POLL_MS`].
#[derive(Debug)]
pub struct InstallRun<C> {
    child: C,
    last: Option<StatusToken>,
    last_step: Option<Step>,
    percent: u8,
}

/// Start the installer: `/bin/sh` [`INSTALLER`] in `/opt/neuraldsp`, as the stock QProcess
/// runner at VA 0x004e6830 starts `sysupdate_run.sh`. A missing [`INSTALLER`] refuses; there
/// is no fall-back to the stock script.
///
/// Owned policy: the stale status file is removed first (stock only removes it at boot,
/// `S80drvinit:282`), so every token read belongs to this run.
pub fn start<R: CommandRunner>(
    runner: &mut R,
    files: &mut impl Files,
) -> Result<InstallRun<R::Child>, InstallError> {
    if files.size(Path::new(INSTALLER))?.is_none() {
        return Err(InstallError::NoInstaller(INSTALLER));
    }
    if files
        .size(Path::new(paths::UPDATE_BIN))?
        .is_none_or(|size| size == 0)
    {
        return Err(InstallError::NoPackage(paths::UPDATE_BIN));
    }
    files.remove(Path::new(paths::STATUS_FILE))?;
    files.sync()?;
    let command = Command::new("/bin/sh", [INSTALLER]).cwd(paths::SCRIPT_DIR);
    let child = runner.spawn(&command)?;
    Ok(InstallRun {
        child,
        last: None,
        last_step: None,
        percent: 0,
    })
}

impl<C: ChildProcess> InstallRun<C> {
    pub fn poll(&mut self, files: &mut impl Files) -> Result<InstallPoll, InstallError> {
        // Read the exit first, then the file, so the final token is never missed.
        let exit = self.child.try_wait()?;
        if let Some(token) = read_status(files)? {
            if let StatusToken::Step(step) = token {
                self.last_step = Some(step);
            }
            self.last = Some(token);
            self.percent = token.percent().unwrap_or(self.percent);
        }
        let Some(exit) = exit else {
            return Ok(InstallPoll::Running {
                last: self.last,
                percent: self.percent,
            });
        };
        if exit == ExitStatus::Code(0) && self.last == Some(StatusToken::Complete) {
            copy_changelog(files)?;
            return Ok(InstallPoll::Finished(InstallOutcome::Installed));
        }
        let code = match exit {
            ExitStatus::Code(code) => Some(code),
            ExitStatus::Signal(_) => None,
        };
        let failure = code.and_then(InstallerFailure::from_code);
        Ok(InstallPoll::Finished(InstallOutcome::Failed {
            code,
            failure,
            last: self.last,
            status_consistent: failure
                .is_some_and(|failure| status_fits(failure, self.last, self.last_step)),
            text: code.map_or_else(|| "Installer crashed".into(), failure_text),
        }))
    }
}

/// Reads the status file. A file over `STATUS_MAX_BYTES` is not read at all.
fn read_status(files: &mut impl Files) -> Result<Option<StatusToken>, FileError> {
    let path = Path::new(paths::STATUS_FILE);
    let raw = match files.size(path)? {
        Some(len) if len <= STATUS_MAX_BYTES as u64 => files.read(path)?,
        _ => None,
    };
    Ok(raw.as_deref().and_then(parse_status))
}

/// The last token and step must match what the script writes before this exit.
fn status_fits(
    failure: InstallerFailure,
    last: Option<StatusToken>,
    last_step: Option<Step>,
) -> bool {
    let token_fits = matches!(
        (failure.published_on_exit(), last),
        (PublishedOnExit::Retry, Some(StatusToken::Retry))
            | (PublishedOnExit::Failure, Some(StatusToken::Failure))
            | (
                PublishedOnExit::RetryOrFailure,
                Some(StatusToken::Retry | StatusToken::Failure)
            )
            | (PublishedOnExit::Nothing, None)
    );
    token_fits && last_step <= failure.last_step_before()
}

/// Stock state 4 (VA 0x004e4708): replace `changelog_boot` with `changelog`, then sync.
fn copy_changelog(files: &mut impl Files) -> Result<(), FileError> {
    if let Some(log) = files.read(Path::new(paths::CHANGELOG))? {
        files.write(Path::new(paths::CHANGELOG_BOOT), &log)?;
        files.sync()?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake::{FakeChild, MemFiles, ScriptedRunner};

    const SCRIPT: &str = "/bin/sh /media/p4/tetrad/launcher/tetrad-update.sh";

    fn setup() -> (ScriptedRunner, MemFiles, FakeChild) {
        let files = MemFiles::default();
        files.put(INSTALLER, "#!/bin/sh\n");
        files.put(paths::UPDATE_BIN, vec![0x1f, 0x8b]);
        files.put(paths::STATUS_FILE, "UPDATER_RETRY\n");
        files.put(paths::CHANGELOG, "notes");
        let child = FakeChild::default();
        let mut runner = ScriptedRunner::default();
        runner.expect_spawn(SCRIPT, child.clone());
        (runner, files, child)
    }

    #[test]
    fn complete_run_reports_progress_and_copies_changelog() {
        let (mut runner, mut files, child) = setup();
        let mut run = start(&mut runner, &mut files).unwrap();
        assert_eq!(
            runner.calls[0].cwd.as_deref(),
            Some(Path::new("/opt/neuraldsp"))
        );
        // The stale RETRY from an earlier run is gone before the script starts.
        assert_eq!(
            run.poll(&mut files).unwrap(),
            InstallPoll::Running {
                last: None,
                percent: 0
            }
        );
        files.put(paths::STATUS_FILE, "UPDATER_STEP_7\n");
        assert_eq!(
            run.poll(&mut files).unwrap(),
            InstallPoll::Running {
                last: Some(StatusToken::Step(Step::new(7).unwrap())),
                percent: 50
            }
        );
        files.put(paths::STATUS_FILE, "UPDATER_COMPLETE\n");
        child.exit(ExitStatus::Code(0));
        assert_eq!(
            run.poll(&mut files).unwrap(),
            InstallPoll::Finished(InstallOutcome::Installed)
        );
        assert_eq!(files.get(paths::CHANGELOG_BOOT).unwrap(), b"notes");
    }

    #[test]
    fn failure_exit_maps_code_and_stock_text() {
        let (mut runner, mut files, child) = setup();
        let mut run = start(&mut runner, &mut files).unwrap();
        files.put(paths::STATUS_FILE, "UPDATER_FAILURE\n");
        child.exit(ExitStatus::Code(1));
        let InstallPoll::Finished(InstallOutcome::Failed { failure, text, .. }) =
            run.poll(&mut files).unwrap()
        else {
            panic!("expected failure");
        };
        assert_eq!(failure, Some(InstallerFailure::DataTranslation));
        assert_eq!(text, "Server authentication failure (error code 1)");
        // Exit 0 without COMPLETE is not an install.
        let (mut runner, mut files, child) = setup();
        let mut run = start(&mut runner, &mut files).unwrap();
        child.exit(ExitStatus::Code(0));
        assert!(matches!(
            run.poll(&mut files).unwrap(),
            InstallPoll::Finished(InstallOutcome::Failed { code: Some(0), .. })
        ));
    }

    /// No Tetrad installer, no install: nothing spawns (no fall-back to the stock script, which
    /// would drop the boot hook) and the status file stays.
    #[test]
    fn a_missing_tetrad_installer_refuses_the_install() {
        let mut files = MemFiles::default();
        files.put(paths::UPDATE_BIN, vec![0x1f, 0x8b]);
        files.put(paths::STATUS_FILE, "UPDATER_RETRY\n");
        let mut runner = ScriptedRunner::default();
        assert_eq!(
            start(&mut runner, &mut files).unwrap_err(),
            InstallError::NoInstaller(INSTALLER)
        );
        assert!(runner.calls.is_empty());
        assert!(files.get(paths::STATUS_FILE).is_some());
    }

    /// The installer's own exits: 218 after STEP_12 and FAILURE (the step before the marker
    /// flip failed), 220 before any token (refused). Both have Tetrad texts; stock codes keep
    /// the stock text.
    #[test]
    fn tetrad_installer_exits_map_to_their_own_failures() {
        let InstallOutcome::Failed {
            failure,
            status_consistent,
            text,
            ..
        } = finish(&["UPDATER_STEP_12\n", "UPDATER_FAILURE\n"], 218)
        else {
            panic!("expected failure");
        };
        assert_eq!(failure, Some(InstallerFailure::HookReapply));
        assert!(status_consistent);
        assert!(
            text.starts_with("Tetrad could not keep its boot hook"),
            "{text}"
        );
        assert!(text.ends_with("(error code 218)"), "{text}");
        let InstallOutcome::Failed {
            failure,
            status_consistent,
            text,
            ..
        } = finish(&[], 220)
        else {
            panic!("expected failure");
        };
        assert_eq!(failure, Some(InstallerFailure::Refused));
        assert!(status_consistent);
        assert!(text.ends_with("(error code 220)"), "{text}");
        assert!(!matches!(
            finish(&["UPDATER_START\n"], 220),
            InstallOutcome::Failed {
                status_consistent: true,
                ..
            }
        ));
        assert_eq!(
            failure_text(216),
            "Unable to copy dtb file onto system partition (error code 216)"
        );
    }

    #[test]
    fn strict_status_tokens() {
        assert_eq!(
            parse_status(b"UPDATER_STEP_15\n"),
            Some(StatusToken::Step(Step::new(15).unwrap()))
        );
        assert_eq!(
            StatusToken::Step(Step::new(14).unwrap()).percent(),
            Some(100)
        );
        for bad in [
            &b"UPDATER_STEP_0"[..],
            b"UPDATER_STEP_07",
            b"UPDATER_STEP_16",
            b"",
            b"UPDATER_STEP_3UPDATER_FAILURE",
        ] {
            assert_eq!(parse_status(bad), None);
        }
        assert_eq!(InstallerFailure::from_code(217).unwrap().code(), 217);
        assert_eq!(
            InstallerFailure::from_code(202)
                .unwrap()
                .published_on_exit(),
            PublishedOnExit::RetryOrFailure
        );
    }

    /// One install that publishes `writes` in order, then exits with `exit`.
    fn finish(writes: &[&str], exit: i32) -> InstallOutcome {
        let (mut runner, mut files, child) = setup();
        let mut run = start(&mut runner, &mut files).unwrap();
        for write in writes {
            files.put(paths::STATUS_FILE, *write);
            run.poll(&mut files).unwrap();
        }
        child.exit(ExitStatus::Code(exit));
        let InstallPoll::Finished(outcome) = run.poll(&mut files).unwrap() else {
            panic!("expected the run to finish");
        };
        outcome
    }

    #[test]
    fn oversized_status_file_is_ignored() {
        let (mut runner, mut files, _child) = setup();
        let mut run = start(&mut runner, &mut files).unwrap();
        let oversized = format!("UPDATER_STEP_3{}", " ".repeat(STATUS_MAX_BYTES));
        files.put(paths::STATUS_FILE, oversized.into_bytes());
        assert_eq!(
            run.poll(&mut files).unwrap(),
            InstallPoll::Running {
                last: None,
                percent: 0
            }
        );
    }

    #[test]
    fn retry_follows_the_published_token_not_the_exit_code() {
        assert!(!finish(&["UPDATER_FAILURE\n"], 202).retry_advised());
        assert!(finish(&["UPDATER_RETRY\n"], 202).retry_advised());
        assert!(finish(&["UPDATER_RETRY\n"], 201).retry_advised());
    }

    #[test]
    fn status_must_fit_the_failure_exit() {
        let consistent = |outcome: InstallOutcome| {
            matches!(
                outcome,
                InstallOutcome::Failed {
                    status_consistent: true,
                    ..
                }
            )
        };
        assert!(consistent(finish(&["UPDATER_RETRY\n"], 201)));
        assert!(!consistent(finish(&["UPDATER_FAILURE\n"], 201)));
        assert!(consistent(finish(
            &["UPDATER_STEP_3\n", "UPDATER_FAILURE\n"],
            210
        )));
        assert!(!consistent(finish(
            &["UPDATER_STEP_9\n", "UPDATER_FAILURE\n"],
            210
        )));
    }
}

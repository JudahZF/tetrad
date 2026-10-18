//! Linux procfs process source. Reads `<root>/<pid>/stat` files with bounded I/O.
//! No signal, kill, reboot, hardware-watchdog or device access exists here.
//!
//! A snapshot is a sequence of separate reads, not an atomic table. The adapter
//! reports what it actually read and refuses to turn root, permission, parse,
//! budget or partial-walk failures into absence. Only a listed entry that
//! disappeared beneath a still-present root (`ENOENT`/`ESRCH`) is an exit.
//!
//! The root is a **trusted** procfs mount or a synthetic test tree. Every `stat`
//! path is checked to be a regular file before it is opened, so a FIFO, socket or
//! directory placed there is refused instead of blocking. That check and the open
//! are separate syscalls; a hostile root that swaps the file between them can
//! still block a read. No bounded wall-clock promise is made for a snapshot.

use std::{
    fs, io,
    io::Read,
    num::{NonZeroU32, NonZeroUsize},
    path::{Path, PathBuf},
};

use thiserror::Error;

use crate::process::{MAX_STAT_BYTES, ParseError, ProcessRecord, ProcessSource, ProcessTarget};

/// `ESRCH`: Linux returns it from `/proc/<pid>/stat` once the task is gone.
const ESRCH: i32 = 3;

/// Observation scope. `Pid` reads one file and is the only coherent scope; `System`
/// walks every numeric directory and is inherently non-atomic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    System,
    Pid(NonZeroU32),
}

impl Scope {
    /// Name targets need the whole table; identity targets need exactly one file.
    pub fn for_target(target: &ProcessTarget) -> Self {
        match target {
            ProcessTarget::Name(_) => Self::System,
            ProcessTarget::Identity(identity) => Self::Pid(identity.pid),
        }
    }
}

/// Work and memory limits for one snapshot. `records` bounds parsed records kept
/// in memory. `entries` bounds every directory entry visited during a system walk,
/// numeric or not, and therefore also bounds `stat` read attempts. Exhausting
/// either limit fails the snapshot; a partial walk is never reported as absence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScanBudget {
    records: NonZeroUsize,
    entries: NonZeroUsize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[error("entry budget {entries} is smaller than record budget {records}")]
pub struct ScanBudgetError {
    records: NonZeroUsize,
    entries: NonZeroUsize,
}

impl ScanBudget {
    /// `entries` must be at least `records`, otherwise the record budget could never be met.
    pub fn new(records: NonZeroUsize, entries: NonZeroUsize) -> Result<Self, ScanBudgetError> {
        if entries < records {
            return Err(ScanBudgetError { records, entries });
        }
        Ok(Self { records, entries })
    }

    pub fn records(&self) -> NonZeroUsize {
        self.records
    }

    pub fn entries(&self) -> NonZeroUsize {
        self.entries
    }
}

/// What the last successful snapshot actually observed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SnapshotReport {
    /// Directory entries visited in a system walk, numeric or not (zero for `Scope::Pid`).
    pub visited: usize,
    /// Numeric directory entries listed (one for `Scope::Pid`, whether or not it exists).
    pub candidates: usize,
    /// Records parsed and returned.
    pub records: usize,
    /// Listed entries whose stat file was gone by the time it was read.
    pub vanished: usize,
}

#[derive(Debug, Error)]
pub enum ProcfsError {
    #[error("proc root {path} is unusable: {source}")]
    Root {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("proc root {path} is not a directory")]
    RootNotDirectory { path: PathBuf },
    #[error("cannot list {path}: {source}")]
    Enumerate {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("stat for PID {pid} is not a regular file")]
    NotRegular { pid: NonZeroU32 },
    #[error("cannot read stat for PID {pid}: {source}")]
    Read {
        pid: NonZeroU32,
        #[source]
        source: io::Error,
    },
    #[error("stat for PID {pid} is unusable: {source}")]
    Parse {
        pid: NonZeroU32,
        #[source]
        source: ParseError,
    },
    #[error("stat under directory {directory} reports PID {reported}")]
    PidMismatch {
        directory: NonZeroU32,
        reported: NonZeroU32,
    },
    #[error("more than {maximum} processes in scope")]
    TooMany { maximum: NonZeroUsize },
    #[error("more than {maximum} directory entries visited; scan abandoned")]
    TooManyEntries { maximum: NonZeroUsize },
}

/// Bounded procfs reader. One instance owns its record and read buffers; the
/// returned slice is valid until the next snapshot.
#[derive(Debug)]
pub struct ProcfsSource {
    root: PathBuf,
    scope: Scope,
    budget: ScanBudget,
    records: Vec<ProcessRecord>,
    buffer: Vec<u8>,
    report: SnapshotReport,
}

impl ProcfsSource {
    /// `root` is normally `/proc`; tests point it at a synthetic tree. The root is
    /// validated on every snapshot, not here, so a root that disappears later is
    /// still reported as a root failure rather than as process absence.
    pub fn new(root: impl Into<PathBuf>, scope: Scope, budget: ScanBudget) -> Self {
        Self {
            root: root.into(),
            scope,
            budget,
            records: Vec::new(),
            buffer: Vec::with_capacity(MAX_STAT_BYTES + 1),
            report: SnapshotReport::default(),
        }
    }

    pub fn scope(&self) -> Scope {
        self.scope
    }

    pub fn budget(&self) -> ScanBudget {
        self.budget
    }

    /// Report for the last **successful** snapshot; a failed snapshot leaves it unchanged.
    pub fn report(&self) -> SnapshotReport {
        self.report
    }

    /// The root must exist and be a directory. Checked before the first read and
    /// again whenever a stat file is missing, so root loss is never absence.
    fn check_root(&self) -> Result<(), ProcfsError> {
        let metadata = fs::metadata(&self.root).map_err(|source| ProcfsError::Root {
            path: self.root.clone(),
            source,
        })?;
        if !metadata.is_dir() {
            return Err(ProcfsError::RootNotDirectory {
                path: self.root.clone(),
            });
        }
        Ok(())
    }

    fn take(&mut self) -> Result<SnapshotReport, ProcfsError> {
        self.records.clear();
        self.check_root()?;
        let mut report = SnapshotReport::default();
        match self.scope {
            Scope::Pid(pid) => {
                report.candidates = 1;
                if let Some(record) = self.read_one(pid)? {
                    self.records.push(record);
                    report.records = 1;
                }
            }
            Scope::System => {
                let entries =
                    fs::read_dir(&self.root).map_err(|source| ProcfsError::Enumerate {
                        path: self.root.clone(),
                        source,
                    })?;
                for entry in entries {
                    let entry = entry.map_err(|source| ProcfsError::Enumerate {
                        path: self.root.clone(),
                        source,
                    })?;
                    report.visited += 1;
                    if report.visited > self.budget.entries.get() {
                        return Err(ProcfsError::TooManyEntries {
                            maximum: self.budget.entries,
                        });
                    }
                    let Some(pid) = numeric_name(&entry.file_name()) else {
                        continue;
                    };
                    report.candidates += 1;
                    match self.read_one(pid)? {
                        Some(record) => {
                            if self.records.len() == self.budget.records.get() {
                                return Err(ProcfsError::TooMany {
                                    maximum: self.budget.records,
                                });
                            }
                            self.records.push(record);
                            report.records += 1;
                        }
                        None => report.vanished += 1,
                    }
                }
            }
        }
        Ok(report)
    }

    /// `Ok(None)` only for a task that is gone (`ENOENT`/`ESRCH`) while the root is
    /// still present. Every other failure, including root loss, is an error.
    fn read_one(&mut self, pid: NonZeroU32) -> Result<Option<ProcessRecord>, ProcfsError> {
        let path = self.root.join(pid.to_string()).join("stat");
        match fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.file_type().is_file() => {}
            Ok(_) => return Err(ProcfsError::NotRegular { pid }),
            Err(error) if is_gone(&error) => {
                self.check_root()?;
                return Ok(None);
            }
            Err(source) => return Err(ProcfsError::Read { pid, source }),
        }
        match read_bounded(&path, &mut self.buffer) {
            Ok(()) => {}
            Err(error) if is_gone(&error) => {
                self.check_root()?;
                return Ok(None);
            }
            Err(source) => return Err(ProcfsError::Read { pid, source }),
        }
        let record = ProcessRecord::parse_stat(&self.buffer)
            .map_err(|source| ProcfsError::Parse { pid, source })?;
        if record.identity().pid != pid {
            return Err(ProcfsError::PidMismatch {
                directory: pid,
                reported: record.identity().pid,
            });
        }
        Ok(Some(record))
    }
}

impl ProcessSource for ProcfsSource {
    type Error = ProcfsError;

    fn snapshot(&mut self) -> Result<&[ProcessRecord], Self::Error> {
        self.report = self.take()?;
        Ok(&self.records)
    }
}

/// Read at most `MAX_STAT_BYTES + 1` bytes so an oversized file is detected, not truncated.
fn read_bounded(path: &Path, buffer: &mut Vec<u8>) -> io::Result<()> {
    buffer.clear();
    let mut file = fs::File::open(path)?;
    let limit = u64::try_from(MAX_STAT_BYTES + 1).expect("budget fits u64");
    file.by_ref().take(limit).read_to_end(buffer)?;
    Ok(())
}

fn is_gone(error: &io::Error) -> bool {
    error.kind() == io::ErrorKind::NotFound || error.raw_os_error() == Some(ESRCH)
}

/// Only all-digit names are tasks; `self`, `thread-self`, `sys` and dotfiles are skipped.
fn numeric_name(name: &std::ffi::OsStr) -> Option<NonZeroU32> {
    let text = name.to_str()?;
    if text.is_empty() || !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    text.parse().ok().and_then(NonZeroU32::new)
}

#[cfg(test)]
pub(crate) mod synthetic {
    //! Minimal synthetic procfs tree under the host temporary directory.
    use std::{
        fs,
        path::{Path, PathBuf},
        sync::atomic::{AtomicUsize, Ordering},
    };

    use crate::process::fixture;

    static COUNTER: AtomicUsize = AtomicUsize::new(0);

    /// Non-numeric entries every tree contains; a budget must account for them.
    pub const NOISE_ENTRIES: usize = 5;

    pub struct Tree {
        pub root: PathBuf,
    }

    impl Tree {
        pub fn new() -> Self {
            let root = std::env::temp_dir().join(format!(
                "tetrad-watchdog-{}-{}",
                std::process::id(),
                COUNTER.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir_all(&root).unwrap();
            for name in ["self", "thread-self", "sys", "1a", ".hidden"] {
                fs::create_dir_all(root.join(name).join("x")).unwrap();
            }
            Self { root }
        }

        pub fn dir(&self, pid: u32) -> PathBuf {
            self.root.join(pid.to_string())
        }

        pub fn write_raw(&self, pid: u32, bytes: &[u8]) {
            fs::create_dir_all(self.dir(pid)).unwrap();
            fs::write(self.dir(pid).join("stat"), bytes).unwrap();
        }

        pub fn write(&self, pid: u32, name: &[u8], state: u8, start_ticks: u64) {
            self.write_raw(pid, &fixture(pid, name, state, start_ticks));
        }

        pub fn remove(&self, pid: u32) {
            fs::remove_dir_all(self.dir(pid)).unwrap();
        }

        pub fn remove_root(&self) {
            fs::remove_dir_all(&self.root).unwrap();
        }

        pub fn path(&self) -> &Path {
            &self.root
        }
    }

    impl Drop for Tree {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        synthetic::{NOISE_ENTRIES, Tree},
        *,
    };
    use crate::process::{ProcessIdentity, ProcessName};

    fn budget(records: usize, entries: usize) -> ScanBudget {
        ScanBudget::new(
            NonZeroUsize::new(records).unwrap(),
            NonZeroUsize::new(entries).unwrap(),
        )
        .unwrap()
    }

    fn source(tree: &Tree, scope: Scope, records: usize) -> ProcfsSource {
        ProcfsSource::new(tree.path(), scope, budget(records, 64))
    }

    fn pid(value: u32) -> NonZeroU32 {
        NonZeroU32::new(value).unwrap()
    }

    #[test]
    fn system_scope_lists_numeric_entries_only_and_matches_exact_name() {
        let tree = Tree::new();
        tree.write(1, b"init", b'S', 1);
        tree.write(321, b"ZenUI", b'S', 4_200);
        tree.write(322, b"ZenUI-helper", b'R', 4_201);
        let mut source = source(&tree, Scope::System, 8);
        let records = source.snapshot().unwrap();
        assert_eq!(records.len(), 3);
        let target = ProcessTarget::Name(ProcessName::new(b"ZenUI").unwrap());
        let matched: Vec<_> = records.iter().filter(|r| r.matches(&target)).collect();
        assert_eq!(matched.len(), 1);
        assert_eq!(matched[0].identity().pid, pid(321));
        assert_eq!(
            source.report(),
            SnapshotReport {
                visited: NOISE_ENTRIES + 3,
                candidates: 3,
                records: 3,
                vanished: 0
            }
        );
    }

    #[test]
    fn pid_scope_reads_one_file_and_reports_exit_and_reuse() {
        let tree = Tree::new();
        tree.write(321, b"ZenUI", b'S', 4_200);
        let identity = ProcessIdentity {
            pid: pid(321),
            start_ticks: 4_200,
        };
        let target = ProcessTarget::Identity(identity);
        assert_eq!(Scope::for_target(&target), Scope::Pid(pid(321)));
        let mut source = source(&tree, Scope::Pid(pid(321)), 1);
        assert!(source.snapshot().unwrap()[0].matches(&target));
        // Exit: the directory is gone. This is observed absence, not an error.
        tree.remove(321);
        assert!(source.snapshot().unwrap().is_empty());
        assert_eq!(
            source.report(),
            SnapshotReport {
                visited: 0,
                candidates: 1,
                records: 0,
                vanished: 0
            }
        );
        // PID reuse: same number, new start ticks, never satisfies the pinned identity.
        tree.write(321, b"ZenUI", b'S', 9_999);
        let records = source.snapshot().unwrap();
        assert_eq!(records.len(), 1);
        assert!(!records[0].matches(&target));
        // A zombie entry is present on disk but cannot satisfy the target.
        tree.write(321, b"ZenUI", b'Z', 4_200);
        assert!(!source.snapshot().unwrap()[0].matches(&target));
    }

    #[test]
    fn vanished_entries_are_counted_but_other_failures_are_errors() {
        let tree = Tree::new();
        tree.write(1, b"init", b'S', 1);
        // Listed directory whose stat file is already gone: an exit race, omitted.
        fs::create_dir_all(tree.dir(7)).unwrap();
        let mut source = source(&tree, Scope::System, 8);
        assert_eq!(source.snapshot().unwrap().len(), 1);
        assert_eq!(source.report().vanished, 1);
        assert_eq!(source.report().candidates, 2);

        // A directory in place of the stat file is the wrong kind, not absence.
        fs::create_dir_all(tree.dir(8).join("stat")).unwrap();
        let before = source.report();
        assert!(matches!(
            source.snapshot(),
            Err(ProcfsError::NotRegular { pid }) if pid.get() == 8
        ));
        assert_eq!(source.report(), before);
        tree.remove(8);

        // Malformed and oversized records are parse errors, not absence.
        tree.write_raw(9, b"9 (bad");
        assert!(matches!(
            source.snapshot(),
            Err(ProcfsError::Parse { pid, source: ParseError::Syntax }) if pid.get() == 9
        ));
        let mut huge = fixture_bytes(9);
        huge.resize(MAX_STAT_BYTES + 1, b' ');
        tree.write_raw(9, &huge);
        assert!(matches!(
            source.snapshot(),
            Err(ProcfsError::Parse {
                source: ParseError::TooLong,
                ..
            })
        ));
        tree.remove(9);

        // A file that names another PID is incoherent, not trusted.
        tree.write_raw(10, &fixture_bytes(11));
        assert!(matches!(
            source.snapshot(),
            Err(ProcfsError::PidMismatch { directory, reported })
                if directory.get() == 10 && reported.get() == 11
        ));
        tree.remove(10);

        // Transient: once the bad entries are gone the same source recovers.
        assert_eq!(source.snapshot().unwrap().len(), 1);
    }

    /// Only regular files are opened. A FIFO would block `open`; this uses the
    /// other non-regular kinds `std` can create (socket, symlink) as stand-ins.
    #[cfg(unix)]
    #[test]
    fn non_regular_stat_files_are_refused_before_open() {
        let tree = Tree::new();
        fs::create_dir_all(tree.dir(5)).unwrap();
        let _socket = std::os::unix::net::UnixListener::bind(tree.dir(5).join("stat")).unwrap();
        let mut socketed = source(&tree, Scope::Pid(pid(5)), 1);
        assert!(matches!(
            socketed.snapshot(),
            Err(ProcfsError::NotRegular { pid }) if pid.get() == 5
        ));
        tree.remove(5);

        tree.write(6, b"real", b'S', 1);
        fs::create_dir_all(tree.dir(7)).unwrap();
        std::os::unix::fs::symlink(tree.dir(6).join("stat"), tree.dir(7).join("stat")).unwrap();
        let mut linked = source(&tree, Scope::Pid(pid(7)), 1);
        assert!(matches!(
            linked.snapshot(),
            Err(ProcfsError::NotRegular { pid }) if pid.get() == 7
        ));
    }

    #[test]
    fn record_budget_is_an_explicit_error() {
        let tree = Tree::new();
        for pid in 1..=3 {
            tree.write(pid, b"p", b'S', pid.into());
        }
        let mut source = source(&tree, Scope::System, 2);
        assert!(matches!(
            source.snapshot(),
            Err(ProcfsError::TooMany { maximum }) if maximum.get() == 2
        ));
        let mut exact = source_with(&tree, Scope::System, 3, 64);
        assert_eq!(exact.snapshot().unwrap().len(), 3);
    }

    /// Review 2026-10-08: the record limit alone did not bound scan work. Vanished
    /// and non-numeric entries now count against an explicit entry budget, and a
    /// partial walk is an error, never absence.
    #[test]
    fn entry_budget_bounds_scan_work_including_vanished_candidates() {
        let tree = Tree::new();
        for pid in 1..=32 {
            fs::create_dir_all(tree.dir(pid)).unwrap();
        }
        let mut tight = source_with(&tree, Scope::System, 1, NOISE_ENTRIES + 8);
        assert!(matches!(
            tight.snapshot(),
            Err(ProcfsError::TooManyEntries { maximum }) if maximum.get() == NOISE_ENTRIES + 8
        ));
        assert_eq!(tight.report(), SnapshotReport::default());

        let mut enough = source_with(&tree, Scope::System, 1, NOISE_ENTRIES + 32);
        assert!(enough.snapshot().unwrap().is_empty());
        assert_eq!(
            enough.report(),
            SnapshotReport {
                visited: NOISE_ENTRIES + 32,
                candidates: 32,
                records: 0,
                vanished: 32
            }
        );

        // Non-numeric noise alone can exhaust the budget; a tree of only noise is not "empty".
        let mut noise_only = source_with(&tree, Scope::System, 1, NOISE_ENTRIES - 1);
        assert!(matches!(
            noise_only.snapshot(),
            Err(ProcfsError::TooManyEntries { .. })
        ));

        assert!(
            ScanBudget::new(NonZeroUsize::new(8).unwrap(), NonZeroUsize::new(7).unwrap()).is_err()
        );
    }

    /// Review 2026-10-08: a missing root was reported as exit for PID scope. The
    /// root is checked on every snapshot and again when a stat file is missing.
    #[test]
    fn missing_or_wrong_kind_root_is_an_error_under_both_scopes() {
        let tree = Tree::new();
        let absent = tree.path().join("absent");
        let mut by_pid = ProcfsSource::new(&absent, Scope::Pid(pid(1)), budget(1, 1));
        assert!(matches!(by_pid.snapshot(), Err(ProcfsError::Root { .. })));
        assert_eq!(by_pid.report(), SnapshotReport::default());
        let mut by_name = ProcfsSource::new(&absent, Scope::System, budget(3, 64));
        assert!(matches!(by_name.snapshot(), Err(ProcfsError::Root { .. })));

        // A file in place of the root is not a procfs mount either.
        let file_root = tree.path().join("file");
        fs::write(&file_root, b"x").unwrap();
        let mut file_pid = ProcfsSource::new(&file_root, Scope::Pid(pid(1)), budget(1, 1));
        assert!(matches!(
            file_pid.snapshot(),
            Err(ProcfsError::RootNotDirectory { .. })
        ));

        // Same source: valid missing PID is ordinary absence; root loss later is still an error.
        tree.write(2, b"app", b'S', 1);
        let mut live = source(&tree, Scope::Pid(pid(1)), 1);
        assert!(live.snapshot().unwrap().is_empty());
        assert_eq!(live.report().candidates, 1);
        tree.remove_root();
        assert!(matches!(live.snapshot(), Err(ProcfsError::Root { .. })));
        assert_eq!(live.report().candidates, 1);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn host_procfs_reports_this_process_by_identity() {
        // Reads the test's own `/proc/self`-equivalent entry; no other process is touched.
        let own = NonZeroU32::new(std::process::id()).unwrap();
        let mut source = ProcfsSource::new("/proc", Scope::Pid(own), budget(1, 1));
        let records = source.snapshot().unwrap();
        assert_eq!(records.len(), 1);
        let identity = records[0].identity();
        assert_eq!(identity.pid, own);
        assert!(records[0].matches(&ProcessTarget::Identity(identity)));
        assert!(!records[0].comm().is_empty());
    }

    fn source_with(tree: &Tree, scope: Scope, records: usize, entries: usize) -> ProcfsSource {
        ProcfsSource::new(tree.path(), scope, budget(records, entries))
    }

    fn fixture_bytes(pid: u32) -> Vec<u8> {
        crate::process::fixture(pid, b"app", b'R', 5)
    }
}

//! Injected effects. Services reach processes, files and sockets only through these traits, so
//! offline tests script them and nothing runs for real. [`crate::system`] has std backends.

use std::fmt;
use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use thiserror::Error;
use zeroize::Zeroizing;

/// Bytes that may hold a credential or token, zeroed on drop (a queued sign-in password
/// crosses threads, review r14 S-3). `Debug` prints only the length.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct Secret(Zeroizing<Vec<u8>>);

impl Secret {
    pub fn new(bytes: impl Into<Vec<u8>>) -> Self {
        Self(Zeroizing::new(bytes.into()))
    }

    pub fn expose(&self) -> &[u8] {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Secret(<{} bytes>)", self.0.len())
    }
}

/// Shared stop flag for long work (an update download). Clones share the flag: the event
/// loop keeps one and cancels, the worker's command sees it and kills its child (review r14
/// S-2). Equality is identity.
#[derive(Debug, Clone, Default)]
pub struct CancelToken(Arc<AtomicBool>);

impl CancelToken {
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Relaxed);
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }
}

impl PartialEq for CancelToken {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl Eq for CancelToken {}

/// One child process. Arguments are argv entries; no shell runs unless `program` is a shell.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Command {
    pub program: String,
    pub args: Vec<String>,
    pub cwd: Option<PathBuf>,
    pub env: Vec<(String, String)>,
    /// Written to stdin, then closed. Secrets go here so they never appear in argv.
    pub stdin: Option<Secret>,
}

impl Command {
    pub fn new<S: Into<String>>(program: &str, args: impl IntoIterator<Item = S>) -> Self {
        Self {
            program: program.into(),
            args: args.into_iter().map(Into::into).collect(),
            cwd: None,
            env: Vec::new(),
            stdin: None,
        }
    }

    pub fn cwd(mut self, dir: impl Into<PathBuf>) -> Self {
        self.cwd = Some(dir.into());
        self
    }

    pub fn env(mut self, key: &str, value: &str) -> Self {
        self.env.push((key.into(), value.into()));
        self
    }

    pub fn stdin(mut self, input: Secret) -> Self {
        self.stdin = Some(input);
        self
    }

    /// `program arg…` for logs and test assertions. Never includes stdin.
    pub fn line(&self) -> String {
        std::iter::once(self.program.as_str())
            .chain(self.args.iter().map(String::as_str))
            .collect::<Vec<_>>()
            .join(" ")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitStatus {
    Code(i32),
    /// Killed by a signal: Qt's `CrashExit`.
    Signal(i32),
}

impl ExitStatus {
    pub fn success(self) -> bool {
        self == Self::Code(0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Output {
    pub status: ExitStatus,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

impl Output {
    pub fn code(code: i32) -> Self {
        Self {
            status: ExitStatus::Code(code),
            stdout: Vec::new(),
            stderr: Vec::new(),
        }
    }

    pub fn with_stdout(mut self, stdout: impl Into<Vec<u8>>) -> Self {
        self.stdout = stdout.into();
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum CommandError {
    /// Stock `waitForStarted` failure; ZenUI reports it as exit code 99 (VA 0x004e6830).
    #[error("could not start `{program}`: {reason}")]
    Start { program: String, reason: String },
    #[error("waiting for `{program}` failed: {reason}")]
    Wait { program: String, reason: String },
    #[error("output of `{program}` is larger than {limit} bytes")]
    OutputTooLarge { program: String, limit: usize },
    /// The [`CancelToken`] was set; the child was killed.
    #[error("`{program}` cancelled")]
    Cancelled { program: String },
}

pub trait CommandRunner {
    type Child: ChildProcess;

    /// Run to completion and collect output.
    fn run(&mut self, command: &Command) -> Result<Output, CommandError>;

    /// As [`Self::run`], but kill the child once `cancel` is set and report
    /// [`CommandError::Cancelled`].
    fn run_cancellable(
        &mut self,
        command: &Command,
        cancel: &CancelToken,
    ) -> Result<Output, CommandError>;

    /// Start a long child (the installer) that the caller polls.
    fn spawn(&mut self, command: &Command) -> Result<Self::Child, CommandError>;
}

pub trait ChildProcess {
    /// `None` while the child runs. Never blocks.
    fn try_wait(&mut self) -> Result<Option<ExitStatus>, CommandError>;
}

/// One directory member from [`Files::list_dir`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirEntry {
    pub name: String,
    pub is_dir: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("{op} {path}: {reason}")]
pub struct FileError {
    pub op: &'static str,
    pub path: PathBuf,
    pub reason: String,
}

pub trait Files {
    /// `Ok(None)` when the file does not exist.
    fn read(&mut self, path: &Path) -> Result<Option<Vec<u8>>, FileError>;
    /// At most `max + 1` bytes of `path`: a longer file reads as `max + 1` bytes, so the caller
    /// sees that it is too large without holding all of it. The default reads the whole file.
    fn read_bounded(&mut self, path: &Path, max: u64) -> Result<Option<Vec<u8>>, FileError> {
        let mut data = self.read(path)?;
        if let Some(data) = &mut data {
            data.truncate(usize::try_from(max.saturating_add(1)).unwrap_or(usize::MAX));
        }
        Ok(data)
    }
    /// The last `max` bytes of `path` (all of a shorter file), for log tails. The default reads
    /// the whole file.
    fn read_tail(&mut self, path: &Path, max: u64) -> Result<Option<Vec<u8>>, FileError> {
        let mut data = self.read(path)?;
        if let Some(data) = &mut data {
            let keep = usize::try_from(max).unwrap_or(usize::MAX);
            data.drain(..data.len().saturating_sub(keep));
        }
        Ok(data)
    }
    /// At most `max` bytes of `path` from byte `offset` on (none past the end), for a large
    /// file that goes out in pieces. The default reads the whole file.
    fn read_at(
        &mut self,
        path: &Path,
        offset: u64,
        max: u64,
    ) -> Result<Option<Vec<u8>>, FileError> {
        let data = self.read(path)?;
        Ok(data.map(|data| {
            let start = usize::try_from(offset)
                .unwrap_or(usize::MAX)
                .min(data.len());
            let end = start.saturating_add(usize::try_from(max).unwrap_or(usize::MAX));
            data[start..end.min(data.len())].to_vec()
        }))
    }
    /// Create or truncate.
    fn write(&mut self, path: &Path, data: &[u8]) -> Result<(), FileError>;
    /// Replace `path` in one step, so a power cut leaves the old or the new content, never a mix.
    fn write_atomic(&mut self, path: &Path, data: &[u8]) -> Result<(), FileError> {
        let mut temp = path.as_os_str().to_owned();
        temp.push(".tmp");
        let temp = PathBuf::from(temp);
        self.write(&temp, data)?;
        self.sync()?;
        self.rename(&temp, path)?;
        self.sync()
    }
    fn append(&mut self, path: &Path, data: &[u8]) -> Result<(), FileError>;
    /// A missing file is not an error.
    fn remove(&mut self, path: &Path) -> Result<(), FileError>;
    fn rename(&mut self, from: &Path, to: &Path) -> Result<(), FileError>;
    /// `Ok(None)` when the file does not exist.
    fn size(&mut self, path: &Path) -> Result<Option<u64>, FileError>;
    /// Flush file system buffers, as stock `sync`.
    fn sync(&mut self) -> Result<(), FileError>;
    /// Regular files and directories in `dir`, sorted by name; symlinks and other kinds are
    /// left out. `Ok(None)` when `dir` does not exist.
    fn list_dir(&mut self, dir: &Path) -> Result<Option<Vec<DirEntry>>, FileError>;
    /// `mkdir -p`.
    fn create_dirs(&mut self, dir: &Path) -> Result<(), FileError>;
    /// Remove a directory and everything in it. A missing directory is not an error.
    fn remove_tree(&mut self, dir: &Path) -> Result<(), FileError>;
}

/// One [`Files`] handle for two users that never call it at the same time, such as an HTTP
/// client that writes a download into a file and the updater code that waits for it. Each
/// call borrows the handle for that call only.
impl<T: Files + ?Sized> Files for &std::cell::RefCell<&mut T> {
    fn read(&mut self, path: &Path) -> Result<Option<Vec<u8>>, FileError> {
        self.borrow_mut().read(path)
    }
    fn read_bounded(&mut self, path: &Path, max: u64) -> Result<Option<Vec<u8>>, FileError> {
        self.borrow_mut().read_bounded(path, max)
    }
    fn read_tail(&mut self, path: &Path, max: u64) -> Result<Option<Vec<u8>>, FileError> {
        self.borrow_mut().read_tail(path, max)
    }
    fn read_at(
        &mut self,
        path: &Path,
        offset: u64,
        max: u64,
    ) -> Result<Option<Vec<u8>>, FileError> {
        self.borrow_mut().read_at(path, offset, max)
    }
    fn write(&mut self, path: &Path, data: &[u8]) -> Result<(), FileError> {
        self.borrow_mut().write(path, data)
    }
    fn write_atomic(&mut self, path: &Path, data: &[u8]) -> Result<(), FileError> {
        self.borrow_mut().write_atomic(path, data)
    }
    fn append(&mut self, path: &Path, data: &[u8]) -> Result<(), FileError> {
        self.borrow_mut().append(path, data)
    }
    fn remove(&mut self, path: &Path) -> Result<(), FileError> {
        self.borrow_mut().remove(path)
    }
    fn rename(&mut self, from: &Path, to: &Path) -> Result<(), FileError> {
        self.borrow_mut().rename(from, to)
    }
    fn size(&mut self, path: &Path) -> Result<Option<u64>, FileError> {
        self.borrow_mut().size(path)
    }
    fn sync(&mut self) -> Result<(), FileError> {
        self.borrow_mut().sync()
    }
    fn list_dir(&mut self, dir: &Path) -> Result<Option<Vec<DirEntry>>, FileError> {
        self.borrow_mut().list_dir(dir)
    }
    fn create_dirs(&mut self, dir: &Path) -> Result<(), FileError> {
        self.borrow_mut().create_dirs(dir)
    }
    fn remove_tree(&mut self, dir: &Path) -> Result<(), FileError> {
        self.borrow_mut().remove_tree(dir)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum NetError {
    #[error("timed out")]
    Timeout,
    #[error("host name has no address")]
    NoAddress,
    #[error("{0}")]
    Io(String),
}

pub trait NetIo {
    fn resolve(&mut self, host: &str) -> Result<Vec<IpAddr>, NetError>;
    fn tcp_connect(&mut self, target: SocketAddr, timeout: Duration) -> Result<(), NetError>;
    /// Send one datagram and return the first datagram that `target` sends back.
    fn udp_request(
        &mut self,
        target: SocketAddr,
        payload: &[u8],
        timeout: Duration,
    ) -> Result<Vec<u8>, NetError>;
}

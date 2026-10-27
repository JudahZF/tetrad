//! std backends for the effect traits. They act on the real system, so tests never use them.

use std::io::{self, ErrorKind, Read, Write};
use std::net::{IpAddr, SocketAddr, TcpStream, ToSocketAddrs, UdpSocket};
use std::os::unix::process::ExitStatusExt;
use std::path::Path;
use std::process::{Child, Stdio};
use std::time::{Duration, Instant};

use crate::exec::{
    CancelToken, ChildProcess, Command, CommandError, CommandRunner, DirEntry, ExitStatus,
    FileError, Files, NetError, NetIo, Output,
};

const DEFAULT_OUTPUT_LIMIT: usize = 1 << 20;

/// How often a cancellable run looks at its token.
const CANCEL_POLL: Duration = Duration::from_millis(50);

/// Stdout or stderr above `output_limit` bytes fails the run with `OutputTooLarge`.
#[derive(Debug)]
pub struct SystemRunner {
    pub output_limit: usize,
}

impl Default for SystemRunner {
    fn default() -> Self {
        Self {
            output_limit: DEFAULT_OUTPUT_LIMIT,
        }
    }
}

fn std_command(command: &Command) -> std::process::Command {
    let mut std = std::process::Command::new(&command.program);
    std.args(&command.args);
    std.envs(command.env.iter().map(|(k, v)| (k, v)));
    if let Some(dir) = &command.cwd {
        std.current_dir(dir);
    }
    std
}

fn status(status: std::process::ExitStatus) -> ExitStatus {
    match status.code() {
        Some(code) => ExitStatus::Code(code),
        None => ExitStatus::Signal(status.signal().unwrap_or(0)),
    }
}

/// Keeps at most `limit` bytes. `None` means the pipe held more. A missing pipe is empty output.
/// Output past the limit is drained and dropped, so the child never blocks on a full pipe or
/// dies of SIGPIPE, and `wait` returns when it exits.
fn read_capped(pipe: Option<impl Read>, limit: usize) -> io::Result<Option<Vec<u8>>> {
    let Some(mut pipe) = pipe else {
        return Ok(Some(Vec::new()));
    };
    let mut data = Vec::new();
    let cap = u64::try_from(limit).unwrap_or(u64::MAX).saturating_add(1);
    pipe.by_ref().take(cap).read_to_end(&mut data)?;
    if data.len() <= limit {
        return Ok(Some(data));
    }
    io::copy(&mut pipe, &mut io::sink())?;
    Ok(None)
}

fn pump_joined<T>(joined: std::thread::Result<io::Result<T>>) -> io::Result<T> {
    joined.unwrap_or_else(|_| Err(io::Error::other("pipe thread panicked")))
}

impl SystemRunner {
    fn run_with(
        &mut self,
        command: &Command,
        cancel: Option<&CancelToken>,
    ) -> Result<Output, CommandError> {
        let mut child = std_command(command)
            .stdin(if command.stdin.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| CommandError::Start {
                program: command.program.clone(),
                reason: e.to_string(),
            })?;
        let limit = self.output_limit;
        let (stdin, stdout, stderr) =
            (child.stdin.take(), child.stdout.take(), child.stderr.take());
        let (written, stdout, stderr) = std::thread::scope(|scope| {
            let writer = scope.spawn(move || match (&command.stdin, stdin) {
                (Some(input), Some(mut pipe)) => pipe.write_all(input.expose()),
                _ => Ok(()),
            });
            let out = scope.spawn(move || read_capped(stdout, limit));
            let err = scope.spawn(move || read_capped(stderr, limit));
            // The pipes close when the child exits; a kill closes them early.
            if let Some(cancel) = cancel {
                while !(out.is_finished() && err.is_finished()) {
                    if cancel.is_cancelled() {
                        let _ = child.kill();
                        break;
                    }
                    std::thread::sleep(CANCEL_POLL);
                }
            }
            (
                pump_joined(writer.join()),
                pump_joined(out.join()),
                pump_joined(err.join()),
            )
        });
        let exit = child.wait();
        let wait_failed = |e: io::Error| CommandError::Wait {
            program: command.program.clone(),
            reason: e.to_string(),
        };
        let too_large = || CommandError::OutputTooLarge {
            program: command.program.clone(),
            limit,
        };
        let exit = exit.map_err(wait_failed)?;
        if cancel.is_some_and(CancelToken::is_cancelled) {
            return Err(CommandError::Cancelled {
                program: command.program.clone(),
            });
        }
        written.map_err(wait_failed)?;
        Ok(Output {
            status: status(exit),
            stdout: stdout.map_err(wait_failed)?.ok_or_else(too_large)?,
            stderr: stderr.map_err(wait_failed)?.ok_or_else(too_large)?,
        })
    }
}

impl CommandRunner for SystemRunner {
    type Child = SystemChild;

    fn run(&mut self, command: &Command) -> Result<Output, CommandError> {
        self.run_with(command, None)
    }

    fn run_cancellable(
        &mut self,
        command: &Command,
        cancel: &CancelToken,
    ) -> Result<Output, CommandError> {
        self.run_with(command, Some(cancel))
    }

    /// The child inherits stdout/stderr, so its output reaches the app log as stock `tee` does.
    fn spawn(&mut self, command: &Command) -> Result<SystemChild, CommandError> {
        let child = std_command(command)
            .stdin(Stdio::null())
            .spawn()
            .map_err(|e| CommandError::Start {
                program: command.program.clone(),
                reason: e.to_string(),
            })?;
        Ok(SystemChild {
            program: command.program.clone(),
            child,
        })
    }
}

#[derive(Debug)]
pub struct SystemChild {
    program: String,
    child: Child,
}

impl ChildProcess for SystemChild {
    fn try_wait(&mut self) -> Result<Option<ExitStatus>, CommandError> {
        self.child
            .try_wait()
            .map(|s| s.map(status))
            .map_err(|e| CommandError::Wait {
                program: self.program.clone(),
                reason: e.to_string(),
            })
    }
}

#[derive(Debug, Default)]
pub struct SystemFiles;

fn file_error(op: &'static str, path: &Path) -> impl FnOnce(std::io::Error) -> FileError {
    move |e| FileError {
        op,
        path: path.into(),
        reason: e.to_string(),
    }
}

fn missing_is_none<T>(result: std::io::Result<T>) -> std::io::Result<Option<T>> {
    match result {
        Ok(value) => Ok(Some(value)),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

impl Files for SystemFiles {
    fn read(&mut self, path: &Path) -> Result<Option<Vec<u8>>, FileError> {
        missing_is_none(std::fs::read(path)).map_err(file_error("read", path))
    }

    fn read_bounded(&mut self, path: &Path, max: u64) -> Result<Option<Vec<u8>>, FileError> {
        use std::io::Read as _;
        let read = std::fs::File::open(path).and_then(|file| {
            let mut data = Vec::new();
            file.take(max.saturating_add(1)).read_to_end(&mut data)?;
            Ok(data)
        });
        missing_is_none(read).map_err(file_error("read", path))
    }

    fn read_tail(&mut self, path: &Path, max: u64) -> Result<Option<Vec<u8>>, FileError> {
        use std::io::{Read as _, Seek as _, SeekFrom};
        let read = std::fs::File::open(path).and_then(|mut file| {
            let length = file.metadata()?.len();
            file.seek(SeekFrom::Start(length.saturating_sub(max)))?;
            let mut data = Vec::new();
            file.take(max).read_to_end(&mut data)?;
            Ok(data)
        });
        missing_is_none(read).map_err(file_error("read", path))
    }

    fn write(&mut self, path: &Path, data: &[u8]) -> Result<(), FileError> {
        std::fs::write(path, data).map_err(file_error("write", path))
    }

    fn append(&mut self, path: &Path, data: &[u8]) -> Result<(), FileError> {
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .and_then(|mut file| file.write_all(data))
            .map_err(file_error("append", path))
    }

    fn remove(&mut self, path: &Path) -> Result<(), FileError> {
        missing_is_none(std::fs::remove_file(path))
            .map(drop)
            .map_err(file_error("remove", path))
    }

    fn rename(&mut self, from: &Path, to: &Path) -> Result<(), FileError> {
        std::fs::rename(from, to).map_err(file_error("rename", from))
    }

    fn size(&mut self, path: &Path) -> Result<Option<u64>, FileError> {
        missing_is_none(std::fs::metadata(path))
            .map(|m| m.map(|m| m.len()))
            .map_err(file_error("size", path))
    }

    /// Runs `/bin/sync`, as stock does after each status or flag write.
    fn sync(&mut self) -> Result<(), FileError> {
        let path = Path::new("/bin/sync");
        let done = std::process::Command::new(path)
            .status()
            .map_err(file_error("sync", path))?;
        done.success().then_some(()).ok_or(FileError {
            op: "sync",
            path: path.into(),
            reason: format!("{done}"),
        })
    }

    fn list_dir(&mut self, dir: &Path) -> Result<Option<Vec<DirEntry>>, FileError> {
        let Some(read) =
            missing_is_none(std::fs::read_dir(dir)).map_err(file_error("list", dir))?
        else {
            return Ok(None);
        };
        let mut entries = Vec::new();
        for entry in read {
            let entry = entry.map_err(file_error("list", dir))?;
            // `file_type` does not follow symlinks, so a link is neither a file nor a dir.
            let kind = entry.file_type().map_err(file_error("list", dir))?;
            if kind.is_file() || kind.is_dir() {
                entries.push(DirEntry {
                    name: entry.file_name().to_string_lossy().into_owned(),
                    is_dir: kind.is_dir(),
                });
            }
        }
        entries.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(Some(entries))
    }

    fn create_dirs(&mut self, dir: &Path) -> Result<(), FileError> {
        std::fs::create_dir_all(dir).map_err(file_error("mkdir", dir))
    }

    fn remove_tree(&mut self, dir: &Path) -> Result<(), FileError> {
        missing_is_none(std::fs::remove_dir_all(dir))
            .map(drop)
            .map_err(file_error("remove", dir))
    }
}

#[derive(Debug, Default)]
pub struct SystemNet;

fn net_error(e: std::io::Error) -> NetError {
    match e.kind() {
        ErrorKind::TimedOut | ErrorKind::WouldBlock => NetError::Timeout,
        _ => NetError::Io(e.to_string()),
    }
}

impl NetIo for SystemNet {
    fn resolve(&mut self, host: &str) -> Result<Vec<IpAddr>, NetError> {
        let addresses: Vec<IpAddr> = (host, 0)
            .to_socket_addrs()
            .map_err(net_error)?
            .map(|a| a.ip())
            .collect();
        if addresses.is_empty() {
            return Err(NetError::NoAddress);
        }
        Ok(addresses)
    }

    fn tcp_connect(&mut self, target: SocketAddr, timeout: Duration) -> Result<(), NetError> {
        TcpStream::connect_timeout(&target, timeout)
            .map(drop)
            .map_err(net_error)
    }

    fn udp_request(
        &mut self,
        target: SocketAddr,
        payload: &[u8],
        timeout: Duration,
    ) -> Result<Vec<u8>, NetError> {
        let bind: SocketAddr = match target {
            SocketAddr::V4(_) => "0.0.0.0:0",
            SocketAddr::V6(_) => "[::]:0",
        }
        .parse()
        .map_err(|_| NetError::Io("bad bind address".into()))?;
        let socket = UdpSocket::bind(bind).map_err(net_error)?;
        socket.send_to(payload, target).map_err(net_error)?;
        let deadline = Instant::now() + timeout;
        let mut buffer = [0u8; 512];
        loop {
            let left = deadline
                .checked_duration_since(Instant::now())
                .filter(|d| !d.is_zero())
                .ok_or(NetError::Timeout)?;
            socket.set_read_timeout(Some(left)).map_err(net_error)?;
            let (len, from) = socket.recv_from(&mut buffer).map_err(net_error)?;
            if from == target {
                return Ok(buffer[..len].to_vec());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_at_limit_is_kept() {
        assert_eq!(
            read_capped(Some(&b"abc"[..]), 3).unwrap(),
            Some(b"abc".to_vec())
        );
    }

    /// The seek keeps the end of a long log, a short file whole, and a missing file is `None`.
    #[test]
    fn read_tail_keeps_the_end() {
        let path = std::env::temp_dir().join(format!("tetrad-tail-{}", std::process::id()));
        std::fs::write(&path, b"0123456789").unwrap();
        let mut files = SystemFiles;
        assert_eq!(files.read_tail(&path, 4).unwrap(), Some(b"6789".to_vec()));
        assert_eq!(
            files.read_tail(&path, 64).unwrap(),
            Some(b"0123456789".to_vec())
        );
        std::fs::remove_file(&path).unwrap();
        assert_eq!(files.read_tail(&path, 4).unwrap(), None);
    }

    #[test]
    fn output_past_limit_is_drained_and_refused() {
        assert_eq!(
            read_capped(Some(io::repeat(b'x').take(1 << 20)), 5).unwrap(),
            None
        );
    }
}

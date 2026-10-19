//! Host side of one runner process: spawn, frame I/O on worker threads, and kill.
//!
//! The device loop never blocks here. Frames to the runner go through a bounded queue to a
//! writer thread; frames from the runner arrive on a reader thread and go to `sink`. A full
//! queue, a bad frame or the process exit is reported, never waited for.

use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, SyncSender, TrySendError};

use crate::protocol::{self, HostMessage, MAX_FRAME, RunnerMessage};

/// Frames queued for the writer thread before a send fails.
const WRITE_QUEUE: usize = 256;

/// What a runner process reported.
#[derive(Debug, Clone, PartialEq)]
pub enum RunnerOutput {
    Message(RunnerMessage),
    /// The process ended or broke the protocol; no more output follows.
    Exited(String),
}

#[derive(Debug, thiserror::Error)]
pub enum HostError {
    #[error("start runner: {0}")]
    Spawn(#[from] std::io::Error),
    #[error("runner input queue is full")]
    Full,
    #[error("runner has stopped")]
    Stopped,
    #[error("{0}")]
    Frame(#[from] protocol::FrameError),
}

pub struct RunnerProcess {
    child: Child,
    frames: SyncSender<Vec<u8>>,
}

impl RunnerProcess {
    /// Starts `command` with piped stdin and stdout; stderr is inherited for the device log.
    /// `sink` runs on the reader thread for every message and once for the exit.
    pub fn spawn(
        mut command: Command,
        sink: impl Fn(RunnerOutput) + Send + 'static,
    ) -> Result<Self, HostError> {
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()?;
        let (Some(stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
            let _ = child.kill();
            return Err(HostError::Stopped);
        };
        let (frames, queue) = mpsc::sync_channel(WRITE_QUEUE);
        std::thread::spawn(move || write_frames(stdin, &queue));
        std::thread::spawn(move || read_frames(stdout, &sink));
        Ok(Self { child, frames })
    }

    pub fn send(&self, message: &HostMessage) -> Result<(), HostError> {
        match self.frames.try_send(protocol::encode(message)?) {
            Ok(()) => Ok(()),
            Err(TrySendError::Full(_)) => Err(HostError::Full),
            Err(TrySendError::Disconnected(_)) => Err(HostError::Stopped),
        }
    }

    pub fn id(&self) -> u32 {
        self.child.id()
    }

    /// Kills the process now. The reader thread reports the exit.
    pub fn kill(&mut self) {
        let _ = self.child.kill();
    }

    /// Reaps the process if it has exited; `true` when it is gone.
    pub fn try_reap(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(Some(_)))
    }
}

impl Drop for RunnerProcess {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn write_frames(mut stdin: ChildStdin, queue: &mpsc::Receiver<Vec<u8>>) {
    while let Ok(frame) = queue.recv() {
        if stdin
            .write_all(&frame)
            .and_then(|()| stdin.flush())
            .is_err()
        {
            return;
        }
    }
}

fn read_frames(stdout: impl Read, sink: &impl Fn(RunnerOutput)) {
    let mut reader = BufReader::new(stdout);
    let mut line = Vec::new();
    let reason = loop {
        line.clear();
        match Read::take(&mut reader, MAX_FRAME as u64 + 2).read_until(b'\n', &mut line) {
            Ok(0) => break "runner exited".to_owned(),
            Err(error) => break format!("read from runner: {error}"),
            Ok(_) => {}
        }
        if line.last() != Some(&b'\n') {
            break "runner frame over the size limit".to_owned();
        }
        line.pop();
        match protocol::decode::<RunnerMessage>(&line) {
            Ok(message) => sink(RunnerOutput::Message(message)),
            Err(error) => break format!("bad runner frame: {error}"),
        }
    };
    sink(RunnerOutput::Exited(reason));
}

/// A runner on a thread of this process, over in-memory pipes: for tests and desktop tools.
/// It has the same protocol and limits as a process but no fault isolation.
pub struct RunnerThread {
    frames: Option<std::io::PipeWriter>,
}

impl RunnerThread {
    pub fn spawn(
        source: String,
        limits: crate::runner::Limits,
        sink: impl Fn(RunnerOutput) + Send + 'static,
    ) -> Result<Self, HostError> {
        let (input, frames) = std::io::pipe()?;
        let (output, mut writer) = std::io::pipe()?;
        std::thread::spawn(move || {
            let _ = crate::runner::run(&source, BufReader::new(input), &mut writer, limits);
        });
        std::thread::spawn(move || read_frames(output, &sink));
        Ok(Self {
            frames: Some(frames),
        })
    }

    pub fn send(&mut self, message: &HostMessage) -> Result<(), HostError> {
        let frame = protocol::encode(message)?;
        let frames = self.frames.as_mut().ok_or(HostError::Stopped)?;
        frames.write_all(&frame).map_err(|_| HostError::Stopped)
    }

    /// Closes the runner's input; it exits and the reader reports it.
    pub fn kill(&mut self) {
        self.frames = None;
    }
}

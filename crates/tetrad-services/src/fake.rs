//! Scripted backends for offline tests. Nothing here touches a process, file or socket.
//!
//! `MemFiles` clones share storage, so a scripted HTTP download and the service under test see the
//! same files.

use std::cell::RefCell;
use std::collections::{BTreeMap, VecDeque};
use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

use crate::cloud::session::{TokenStore, TokenStoreError, Tokens};
use crate::exec::{
    CancelToken, ChildProcess, Command, CommandError, CommandRunner, DirEntry, ExitStatus,
    FileError, Files, NetError, NetIo, Output,
};
use crate::http::{HttpClient, HttpError, HttpRequest, HttpResponse};

/// Expects commands in order by [`Command::line`]. An unexpected command fails to start.
#[derive(Debug, Default)]
pub struct ScriptedRunner {
    script: VecDeque<(String, Result<Output, CommandError>)>,
    spawns: VecDeque<(String, FakeChild)>,
    pub calls: Vec<Command>,
}

impl ScriptedRunner {
    pub fn expect(&mut self, line: &str, result: Result<Output, CommandError>) -> &mut Self {
        self.script.push_back((line.into(), result));
        self
    }

    pub fn expect_ok(&mut self, line: &str) -> &mut Self {
        self.expect(line, Ok(Output::code(0)))
    }

    pub fn expect_spawn(&mut self, line: &str, child: FakeChild) -> &mut Self {
        self.spawns.push_back((line.into(), child));
        self
    }

    /// Command lines run so far.
    pub fn lines(&self) -> Vec<String> {
        self.calls.iter().map(Command::line).collect()
    }

    pub fn is_done(&self) -> bool {
        self.script.is_empty() && self.spawns.is_empty()
    }
}

fn unexpected(command: &Command) -> CommandError {
    CommandError::Start {
        program: command.program.clone(),
        reason: format!("unexpected command `{}`", command.line()),
    }
}

impl CommandRunner for ScriptedRunner {
    type Child = FakeChild;

    fn run(&mut self, command: &Command) -> Result<Output, CommandError> {
        self.calls.push(command.clone());
        match self.script.pop_front() {
            Some((line, result)) if line == command.line() => result,
            _ => Err(unexpected(command)),
        }
    }

    /// A set token stands for a kill during the run: the scripted entry is used up and the
    /// run reports [`CommandError::Cancelled`].
    fn run_cancellable(
        &mut self,
        command: &Command,
        cancel: &CancelToken,
    ) -> Result<Output, CommandError> {
        let result = self.run(command);
        if cancel.is_cancelled() {
            return Err(CommandError::Cancelled {
                program: command.program.clone(),
            });
        }
        result
    }

    fn spawn(&mut self, command: &Command) -> Result<FakeChild, CommandError> {
        self.calls.push(command.clone());
        match self.spawns.pop_front() {
            Some((line, child)) if line == command.line() => Ok(child),
            _ => Err(unexpected(command)),
        }
    }
}

/// A child whose exit the test sets through a clone.
#[derive(Debug, Clone, Default)]
pub struct FakeChild(Rc<RefCell<Option<ExitStatus>>>);

impl FakeChild {
    pub fn exit(&self, status: ExitStatus) {
        *self.0.borrow_mut() = Some(status);
    }
}

impl ChildProcess for FakeChild {
    fn try_wait(&mut self) -> Result<Option<ExitStatus>, CommandError> {
        Ok(*self.0.borrow())
    }
}

#[derive(Debug, Clone, Default)]
pub struct MemFiles {
    files: Rc<RefCell<BTreeMap<PathBuf, Vec<u8>>>>,
    syncs: Rc<RefCell<u32>>,
    /// Paths whose `remove` fails (and keeps the file).
    stuck: Rc<RefCell<Vec<PathBuf>>>,
}

impl MemFiles {
    pub fn put(&self, path: &str, data: impl Into<Vec<u8>>) {
        self.files.borrow_mut().insert(path.into(), data.into());
    }

    pub fn get(&self, path: &str) -> Option<Vec<u8>> {
        self.files.borrow().get(Path::new(path)).cloned()
    }

    pub fn syncs(&self) -> u32 {
        *self.syncs.borrow()
    }

    /// Every later `remove` of `path` fails.
    pub fn fail_remove(&self, path: &str) {
        self.stuck.borrow_mut().push(path.into());
    }
}

impl Files for MemFiles {
    fn read(&mut self, path: &Path) -> Result<Option<Vec<u8>>, FileError> {
        Ok(self.files.borrow().get(path).cloned())
    }

    fn write(&mut self, path: &Path, data: &[u8]) -> Result<(), FileError> {
        self.files.borrow_mut().insert(path.into(), data.to_vec());
        Ok(())
    }

    fn append(&mut self, path: &Path, data: &[u8]) -> Result<(), FileError> {
        self.files
            .borrow_mut()
            .entry(path.into())
            .or_default()
            .extend_from_slice(data);
        Ok(())
    }

    fn remove(&mut self, path: &Path) -> Result<(), FileError> {
        if self.stuck.borrow().iter().any(|stuck| stuck == path) {
            return Err(FileError {
                op: "remove",
                path: path.into(),
                reason: "scripted failure".into(),
            });
        }
        self.files.borrow_mut().remove(path);
        Ok(())
    }

    fn rename(&mut self, from: &Path, to: &Path) -> Result<(), FileError> {
        let data = self.files.borrow_mut().remove(from).ok_or(FileError {
            op: "rename",
            path: from.into(),
            reason: "missing".into(),
        })?;
        self.files.borrow_mut().insert(to.into(), data);
        Ok(())
    }

    fn size(&mut self, path: &Path) -> Result<Option<u64>, FileError> {
        Ok(self.files.borrow().get(path).map(|d| d.len() as u64))
    }

    fn sync(&mut self) -> Result<(), FileError> {
        *self.syncs.borrow_mut() += 1;
        Ok(())
    }

    /// Directories exist only as prefixes of stored files.
    fn list_dir(&mut self, dir: &Path) -> Result<Option<Vec<DirEntry>>, FileError> {
        let files = self.files.borrow();
        let mut entries: Vec<DirEntry> = Vec::new();
        for path in files.keys() {
            let Ok(rest) = path.strip_prefix(dir) else {
                continue;
            };
            let mut parts = rest.components();
            let Some(first) = parts.next() else {
                continue;
            };
            let name = first.as_os_str().to_string_lossy().into_owned();
            let is_dir = parts.next().is_some();
            if !entries.iter().any(|entry| entry.name == name) {
                entries.push(DirEntry { name, is_dir });
            }
        }
        if entries.is_empty() {
            return Ok(None);
        }
        entries.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(Some(entries))
    }

    fn create_dirs(&mut self, _dir: &Path) -> Result<(), FileError> {
        Ok(())
    }

    fn remove_tree(&mut self, dir: &Path) -> Result<(), FileError> {
        self.files
            .borrow_mut()
            .retain(|path, _| !path.starts_with(dir));
        Ok(())
    }
}

/// Answers requests in order. Downloads write the scripted body into the shared [`MemFiles`].
#[derive(Debug, Default)]
pub struct ScriptedHttp {
    script: VecDeque<Result<HttpResponse, HttpError>>,
    pub files: MemFiles,
    pub requests: Vec<HttpRequest>,
}

impl ScriptedHttp {
    /// Downloads land in `files`, which the service under test also holds.
    pub fn sharing(files: MemFiles) -> Self {
        Self {
            files,
            ..Self::default()
        }
    }

    pub fn respond(&mut self, status: u16, body: impl Into<Vec<u8>>) -> &mut Self {
        self.script.push_back(Ok(HttpResponse {
            status,
            body: body.into(),
        }));
        self
    }

    pub fn fail(&mut self, error: HttpError) -> &mut Self {
        self.script.push_back(Err(error));
        self
    }

    /// A cancelled request uses up its scripted answer and fails, as a killed transfer does.
    fn next(&mut self, request: &HttpRequest) -> Result<HttpResponse, HttpError> {
        self.requests.push(request.clone());
        let answer = self
            .script
            .pop_front()
            .unwrap_or_else(|| Err(HttpError::Transport("no scripted response".into())));
        if request
            .cancel
            .as_ref()
            .is_some_and(CancelToken::is_cancelled)
        {
            return Err(HttpError::Cancelled);
        }
        answer
    }
}

impl HttpClient for ScriptedHttp {
    fn send(&mut self, request: &HttpRequest) -> Result<HttpResponse, HttpError> {
        self.next(request)
    }

    fn download(&mut self, request: &HttpRequest, dest: &Path) -> Result<u16, HttpError> {
        let response = self.next(request)?;
        self.files
            .write(dest, &response.body)
            .map_err(|e| HttpError::Transport(e.to_string()))?;
        Ok(response.status)
    }
}

/// Scripted name, TCP and UDP results, consumed in order per kind.
#[derive(Debug, Default)]
pub struct ScriptedNet {
    pub names: VecDeque<Result<Vec<IpAddr>, NetError>>,
    pub tcp: VecDeque<Result<(), NetError>>,
    pub udp: VecDeque<Result<Vec<u8>, NetError>>,
    pub log: Vec<String>,
}

impl NetIo for ScriptedNet {
    fn resolve(&mut self, host: &str) -> Result<Vec<IpAddr>, NetError> {
        self.log.push(format!("resolve {host}"));
        self.names.pop_front().unwrap_or(Err(NetError::NoAddress))
    }

    fn tcp_connect(&mut self, target: SocketAddr, timeout: Duration) -> Result<(), NetError> {
        self.log
            .push(format!("tcp {target} {}ms", timeout.as_millis()));
        self.tcp.pop_front().unwrap_or(Err(NetError::Timeout))
    }

    fn udp_request(
        &mut self,
        target: SocketAddr,
        payload: &[u8],
        _timeout: Duration,
    ) -> Result<Vec<u8>, NetError> {
        self.log.push(format!("udp {target} {}B", payload.len()));
        self.udp.pop_front().unwrap_or(Err(NetError::Timeout))
    }
}

/// In-memory account state, as tetrad-settings keeps it in `AccountConfig.ini`.
#[derive(Debug, Default)]
pub struct MemTokens {
    pub tokens: Option<Tokens>,
    pub username: Option<String>,
}

impl TokenStore for MemTokens {
    fn load(&mut self) -> Result<Option<Tokens>, TokenStoreError> {
        Ok(self.tokens.clone())
    }

    fn store(&mut self, tokens: &Tokens, username: Option<&str>) -> Result<(), TokenStoreError> {
        self.tokens = Some(tokens.clone());
        if let Some(name) = username {
            self.username = Some(name.to_owned());
        }
        Ok(())
    }

    fn clear(&mut self) -> Result<(), TokenStoreError> {
        self.tokens = None;
        self.username = None;
        Ok(())
    }

    fn username(&mut self) -> Result<Option<String>, TokenStoreError> {
        Ok(self.username.clone())
    }
}

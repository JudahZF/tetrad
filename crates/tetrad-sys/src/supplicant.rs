//! Explicit Linux `UnixDatagram` transport for the wpa_supplicant control interface.
//!
//! Recovered shape (ZenUI VA 0x000eb8b0 open, 0x000eba7c close, 0x000ebaac request): one `AF_UNIX/SOCK_DGRAM`
//! socket bound to a private local path and connected to the supplicant control socket; a request sends one
//! command datagram and reads datagrams until the first one that does not begin with `<`. That datagram is the
//! reply; the others are unsolicited events. Stock restarts its 10 s wait after every event and silently truncates
//! oversized datagrams. This transport instead applies one absolute deadline and explicit byte/event budgets, and
//! detects truncation.
//!
//! Correlation rule: at most one request is outstanding, and `request` is synchronous. If a request sent its command
//! but did not consume exactly one reply before its deadline (timeout, late datagram, receive failure, event
//! budget), the transport is poisoned: a late reply could otherwise satisfy a later, different request. A poisoned
//! transport only closes; open a new one at a fresh local path.
//!
//! Path ownership: `open` records the device/inode identity of the socket node it bound. `close` and `Drop` unlink
//! the local path only while that identity is still present, so a replacement entry another actor placed at the
//! path is left alone. The check and the unlink are two steps; only a caller-controlled private parent directory
//! makes them race-free. Explicit `close` consumes cleanup ownership, so `Drop` never repeats it.
//!
//! Nothing here decodes, logs or stores payload text beyond returning it to the caller. `Payload` redacts its
//! bytes in `Debug`. Nothing is opened unless a caller names both socket paths and calls `open`.

use std::fmt;
use std::fs;
use std::io;
use std::os::unix::fs::{FileTypeExt, MetadataExt};
use std::os::unix::net::UnixDatagram;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Largest receive capacity a budget may ask for. A UNIX datagram cannot usefully exceed this here.
pub const MAX_CAPACITY: usize = 65_535;
/// Longest command datagram accepted. Owned policy; stock has no explicit bound at the transport layer.
pub const MAX_COMMAND_BYTES: usize = 2048;
/// Longest `request_timeout` or `receive` wait admitted. Owned policy: bounds a hang and keeps every deadline
/// addition finite. Stock waits 10 s per event with no total bound.
pub const MAX_TIMEOUT: Duration = Duration::from_secs(3600);
/// Largest `max_events_per_request` admitted. Owned policy: with `event_capacity` it bounds the bytes one
/// `request` can retain (`max_events_per_request * event_capacity + reply_capacity`). Stock has no event budget.
pub const MAX_EVENTS_PER_REQUEST: usize = 1024;

/// Observed stock buffer sizes (VA 0x0088999c, 0x0088f8f4). Implementation observations, not protocol limits.
pub const STOCK_REPLY_CAPACITY: usize = 1023;
pub const STOCK_EVENT_CAPACITY: usize = 255;

/// Global-interface event prefix (owned classification; stock only tests the first byte for `<`).
const EVENT_PREFIX: &[u8] = b"IFNAME=";

/// Explicit per-transport limits. Every value is required; nothing is defaulted from stock.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Budget {
    /// Largest reply accepted, in bytes (`1..=MAX_CAPACITY`). A longer reply fails the request as `OversizedReply`.
    pub reply_capacity: usize,
    /// Largest event accepted, in bytes (`1..=MAX_CAPACITY`). A longer event is reported as `Receipt::Oversized`.
    pub event_capacity: usize,
    /// Absolute budget for one `request`: send, every interleaved event and the reply (`> 0`, `<= MAX_TIMEOUT`).
    pub request_timeout: Duration,
    /// Events tolerated while waiting for one reply (`<= MAX_EVENTS_PER_REQUEST`). Exceeding it poisons.
    pub max_events_per_request: usize,
}

impl Budget {
    fn validate(self) -> Result<(), Error> {
        if self.reply_capacity == 0 || self.reply_capacity > MAX_CAPACITY {
            return Err(Error::InvalidBudget("reply_capacity"));
        }
        if self.event_capacity == 0 || self.event_capacity > MAX_CAPACITY {
            return Err(Error::InvalidBudget("event_capacity"));
        }
        if self.request_timeout.is_zero() || self.request_timeout > MAX_TIMEOUT {
            return Err(Error::InvalidBudget("request_timeout"));
        }
        if self.max_events_per_request > MAX_EVENTS_PER_REQUEST {
            return Err(Error::InvalidBudget("max_events_per_request"));
        }
        Ok(())
    }

    /// Receive buffer length. One byte above the larger capacity detects truncation (`recv` without `MSG_TRUNC`
    /// returns at most the buffer length), and never less than the longest classification prefix, so the kind
    /// of a truncated datagram is still known.
    fn receive_buffer_len(self) -> usize {
        (self.reply_capacity.max(self.event_capacity) + 1).max(EVENT_PREFIX.len())
    }
}

/// Why a transport refuses further requests. Only `close` is still meaningful.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PoisonReason {
    /// The command was sent, but no reply was accepted before the deadline. One may still arrive, or a datagram
    /// that arrived after the deadline was discarded.
    Timeout,
    /// The command was sent, then `recv` failed. Whether a reply is queued is unknown.
    ReceiveFailure,
    /// The command was sent, then more events than `max_events_per_request` arrived. The reply is still due.
    EventFlood,
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// A `Budget` field, or the `receive` timeout (`"timeout"`), is out of its admitted range.
    #[error("supplicant transport limit `{0}` is out of range")]
    InvalidBudget(&'static str),
    #[error("command must be 1..={MAX_COMMAND_BYTES} bytes without NUL or line breaks")]
    InvalidCommand,
    #[error("local socket path already exists; choose a fresh path")]
    LocalPathExists,
    #[error("bind local control socket: {0}")]
    Bind(#[source] io::Error),
    #[error("connect to supplicant control socket: {0}")]
    Connect(#[source] io::Error),
    #[error("configure control socket: {0}")]
    Configure(#[source] io::Error),
    #[error("send command datagram: {0}")]
    Send(#[source] io::Error),
    #[error("receive control datagram: {0}")]
    Receive(#[source] io::Error),
    #[error("no reply before the request deadline")]
    Timeout,
    #[error("more unsolicited events than the request budget allows")]
    EventFlood,
    #[error("reply exceeds the reply capacity ({at_least}+ bytes)")]
    OversizedReply { at_least: usize },
    #[error("transport is poisoned ({0:?}); close it and open a new local path")]
    Poisoned(PoisonReason),
    #[error("control command was not acknowledged with OK")]
    NotAcknowledged,
    #[error("remove local control socket path: {0}")]
    Cleanup(#[source] io::Error),
}

/// Datagram bytes. `Debug` prints only the length: payloads may carry identifiers or credentials.
#[derive(Clone, PartialEq, Eq)]
pub struct Payload(Vec<u8>);

impl Payload {
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// The datagram bytes, allocated to their own length (not to the receive buffer length).
    pub fn into_bytes(self) -> Vec<u8> {
        self.0
    }
}

impl fmt::Debug for Payload {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Payload({} bytes)", self.0.len())
    }
}

/// One received datagram, after the capacity check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Receipt {
    Complete(Payload),
    /// The datagram was longer than the capacity. The kernel discarded the excess; only the size class is known.
    Oversized {
        at_least: usize,
    },
}

/// Classification of a datagram relative to the correlation rule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Received {
    /// Begins with `<` (stock rule, VA 0x000ebaac) or `IFNAME=` (global-interface prefix; owned policy).
    Event(Receipt),
    /// Anything else, including an empty datagram, which stock also treats as a reply.
    Reply(Receipt),
}

/// Result of one correlated request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Response {
    pub reply: Payload,
    /// Unsolicited events received before the reply, in arrival order.
    pub events: Vec<Receipt>,
}

/// Stock local path shape `/tmp/wpa_ctrl_<pid>-<counter>` (VA 0x000eb8b0), under a caller-chosen directory.
/// The caller owns the counter; a fresh value after every `close` avoids late replies reaching a new socket.
pub fn stock_local_path(dir: &Path, counter: u32) -> PathBuf {
    dir.join(format!("wpa_ctrl_{}-{counter}", std::process::id()))
}

/// Filesystem identity of the socket node `open` created. Unlinking is refused when the path no longer carries it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PathIdentity {
    dev: u64,
    ino: u64,
}

/// One control connection. Use two instances for the stock command/monitor split (VA 0x0088f974).
pub struct Transport {
    socket: UnixDatagram,
    local_path: PathBuf,
    /// `Some` while this transport still owns cleanup of `local_path`; `close` takes it.
    owned_node: Option<PathIdentity>,
    budget: Budget,
    /// Reused receive buffer of `budget.receive_buffer_len()` bytes; payloads are copied out right-sized.
    scratch: Vec<u8>,
    poison: Option<PoisonReason>,
}

impl fmt::Debug for Transport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Transport")
            .field("local_path", &self.local_path)
            .field("owned_node", &self.owned_node)
            .field("budget", &self.budget)
            .field("poison", &self.poison)
            .finish_non_exhaustive()
    }
}

impl Transport {
    /// Bind `local_path`, connect to `remote_path`. Refuses an existing `local_path` instead of unlinking it
    /// (stock unlinks once on `EADDRINUSE`). On any later failure the just-created local path is removed.
    ///
    /// The parent directory of `local_path` must be private to the caller: identity is read after `bind`, and
    /// cleanup checks it again before `unlink`. Another writer in that directory can defeat both steps.
    pub fn open(local_path: &Path, remote_path: &Path, budget: Budget) -> Result<Self, Error> {
        budget.validate()?;
        if fs::symlink_metadata(local_path).is_ok() {
            return Err(Error::LocalPathExists);
        }
        let socket = UnixDatagram::bind(local_path).map_err(|e| {
            if e.kind() == io::ErrorKind::AddrInUse {
                Error::LocalPathExists
            } else {
                Error::Bind(e)
            }
        })?;
        // If the node cannot be identified it is not provably ours, so it is not unlinked either.
        let owned_node = identify_socket_node(local_path).map_err(Error::Bind)?;
        let transport = Self {
            socket,
            local_path: local_path.to_path_buf(),
            owned_node: Some(owned_node),
            budget,
            scratch: vec![0u8; budget.receive_buffer_len()],
            poison: None,
        };
        transport
            .socket
            .connect(remote_path)
            .map_err(Error::Connect)?;
        Ok(transport)
    }

    pub fn budget(&self) -> Budget {
        self.budget
    }

    pub fn local_path(&self) -> &Path {
        &self.local_path
    }

    pub fn poison(&self) -> Option<PoisonReason> {
        self.poison
    }

    /// Send one command and wait for exactly one correlated reply within `budget.request_timeout`.
    /// Events that arrive first are returned in `Response::events`. See the module docs for poisoning.
    ///
    /// The deadline is an acceptance cutoff: a datagram whose `recv` completes after it is discarded and the
    /// request fails as `Timeout`, even when the datagram was the reply. The call itself may still return late
    /// under scheduling pressure; userspace cannot promise a hard wall.
    pub fn request(&mut self, command: &[u8]) -> Result<Response, Error> {
        if let Some(reason) = self.poison {
            return Err(Error::Poisoned(reason));
        }
        if command.is_empty()
            || command.len() > MAX_COMMAND_BYTES
            || command.iter().any(|&b| matches!(b, 0 | b'\n' | b'\r'))
        {
            return Err(Error::InvalidCommand);
        }
        let deadline = deadline_after(self.budget.request_timeout, "request_timeout")?;
        self.send_before(command, deadline)?;

        // From here a reply is owed. Every early exit must poison. A send that completed after the deadline is
        // covered too: the first `receive_before` sees no time left and takes the `Ok(None)` arm.
        let mut events = Vec::new();
        loop {
            match self.receive_before(deadline) {
                Ok(Some(_)) if Instant::now() > deadline => {
                    // Consumed after the cutoff: never accepted, whatever its kind.
                    self.poison = Some(PoisonReason::Timeout);
                    return Err(Error::Timeout);
                }
                Ok(Some(Received::Reply(Receipt::Complete(payload)))) => {
                    return Ok(Response {
                        reply: payload,
                        events,
                    });
                }
                Ok(Some(Received::Reply(Receipt::Oversized { at_least }))) => {
                    // The reply was consumed, so nothing late is owed: not poisoned.
                    return Err(Error::OversizedReply { at_least });
                }
                Ok(Some(Received::Event(receipt))) => {
                    if events.len() >= self.budget.max_events_per_request {
                        self.poison = Some(PoisonReason::EventFlood);
                        return Err(Error::EventFlood);
                    }
                    events.push(receipt);
                }
                Ok(None) => {
                    self.poison = Some(PoisonReason::Timeout);
                    return Err(Error::Timeout);
                }
                Err(e) => {
                    self.poison = Some(PoisonReason::ReceiveFailure);
                    return Err(e);
                }
            }
        }
    }

    /// Wait up to `timeout` (`<= MAX_TIMEOUT`) for one datagram with no request outstanding (monitor use after
    /// `attach`). `None` means nothing arrived; `Duration::ZERO` returns `None` without polling the queue.
    /// This never poisons and never discards: nothing is owed, so a datagram consumed as the timeout elapsed
    /// is still returned.
    pub fn receive(&mut self, timeout: Duration) -> Result<Option<Received>, Error> {
        if let Some(reason) = self.poison {
            return Err(Error::Poisoned(reason));
        }
        if timeout > MAX_TIMEOUT {
            return Err(Error::InvalidBudget("timeout"));
        }
        let deadline = deadline_after(timeout, "timeout")?;
        self.receive_before(deadline)
    }

    /// One datagram that is already queued, without waiting (monitor polling from an event loop after
    /// `attach`). `None` means the queue is empty. Like [`Self::receive`] it never poisons or discards.
    pub fn try_receive(&mut self) -> Result<Option<Received>, Error> {
        if let Some(reason) = self.poison {
            return Err(Error::Poisoned(reason));
        }
        self.socket
            .set_nonblocking(true)
            .map_err(Error::Configure)?;
        let received = self.socket.recv(&mut self.scratch);
        self.socket
            .set_nonblocking(false)
            .map_err(Error::Configure)?;
        match received {
            Ok(len) => Ok(Some(self.classify(len))),
            Err(e) if is_timeout(&e) => Ok(None),
            Err(e) => Err(Error::Receive(e)),
        }
    }

    /// `ATTACH` with acknowledgement (VA 0x000ebd60; attach must succeed at VA 0x0088f974).
    pub fn attach(&mut self) -> Result<Response, Error> {
        self.expect_ok(b"ATTACH")
    }

    /// `DETACH` with acknowledgement (VA 0x000ebd68).
    pub fn detach(&mut self) -> Result<Response, Error> {
        self.expect_ok(b"DETACH")
    }

    fn expect_ok(&mut self, command: &[u8]) -> Result<Response, Error> {
        let response = self.request(command)?;
        if response.reply.as_bytes().trim_ascii_end() == b"OK" {
            Ok(response)
        } else {
            Err(Error::NotAcknowledged)
        }
    }

    /// Remove the local path, then close the socket. Reports a cleanup failure instead of hiding it.
    /// Unlinking first means a late reply addressed to the path fails at the sender rather than queueing.
    /// This is the one cleanup attempt: `Drop` does not retry, whether or not it succeeded. A foreign entry
    /// that replaced the owned socket node is left alone and is not a failure.
    pub fn close(mut self) -> Result<(), Error> {
        self.release_path().map_err(Error::Cleanup)
        // `Drop` closes the descriptor and finds ownership already consumed.
    }

    /// Unlink the owned socket node, once. Ownership is consumed even when the unlink fails.
    fn release_path(&mut self) -> io::Result<()> {
        match self.owned_node.take() {
            Some(own) => remove_own_socket(&self.local_path, own),
            None => Ok(()),
        }
    }

    fn send_before(&mut self, command: &[u8], deadline: Instant) -> Result<(), Error> {
        let remaining = remaining_until(deadline).ok_or(Error::Timeout)?;
        self.socket
            .set_write_timeout(Some(remaining))
            .map_err(Error::Configure)?;
        // A datagram send is all-or-nothing; a send error means nothing is outstanding, so no poison.
        match self.socket.send(command) {
            Ok(n) if n == command.len() => Ok(()),
            Ok(_) => Err(Error::Send(io::Error::other("short datagram send"))),
            Err(e) if is_timeout(&e) => Err(Error::Timeout),
            Err(e) => Err(Error::Send(e)),
        }
    }

    /// One `recv` bounded by the time left before `deadline`. Returns whatever was consumed; the caller decides
    /// whether a datagram that completed after `deadline` is acceptable.
    fn receive_before(&mut self, deadline: Instant) -> Result<Option<Received>, Error> {
        let Some(remaining) = remaining_until(deadline) else {
            return Ok(None);
        };
        self.socket
            .set_read_timeout(Some(remaining))
            .map_err(Error::Configure)?;
        match self.socket.recv(&mut self.scratch) {
            Ok(len) => Ok(Some(self.classify(len))),
            Err(e) if is_timeout(&e) => Ok(None),
            Err(e) => Err(Error::Receive(e)),
        }
    }

    /// The `len` bytes just received into `scratch`, as an event or a reply.
    fn classify(&self, len: usize) -> Received {
        let datagram = &self.scratch[..len];
        // The buffer is at least `EVENT_PREFIX.len()` long, so a truncated datagram still shows its full prefix.
        let is_event = datagram.first() == Some(&b'<') || datagram.starts_with(EVENT_PREFIX);
        let limit = if is_event {
            self.budget.event_capacity
        } else {
            self.budget.reply_capacity
        };
        // `len == scratch.len()` means the kernel may have discarded excess; either way `len > limit` is oversized.
        let receipt = if len > limit {
            Receipt::Oversized { at_least: len }
        } else {
            Receipt::Complete(Payload(datagram.to_vec()))
        };
        if is_event {
            Received::Event(receipt)
        } else {
            Received::Reply(receipt)
        }
    }
}

impl Drop for Transport {
    fn drop(&mut self) {
        // After an explicit `close` ownership is already consumed and this is a no-op. Errors cannot propagate.
        let _ = self.release_path();
    }
}

/// Identity of the socket node at `path`, or an error if the entry is missing or not a socket.
fn identify_socket_node(path: &Path) -> io::Result<PathIdentity> {
    let meta = fs::symlink_metadata(path)?;
    if !meta.file_type().is_socket() {
        return Err(io::Error::other("bound local path is not a socket node"));
    }
    Ok(PathIdentity {
        dev: meta.dev(),
        ino: meta.ino(),
    })
}

/// Unlink `path` only while it still carries the socket node identified at `open`. A missing entry is done;
/// any other entry (foreign file, directory, or a replacement socket) is never ours and is left alone.
/// The metadata read and the unlink are separate syscalls: a writer sharing the parent directory can still
/// swap the entry between them. Private parent directories are the documented requirement.
fn remove_own_socket(path: &Path, own: PathIdentity) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(meta)
            if meta.file_type().is_socket() && meta.dev() == own.dev && meta.ino() == own.ino =>
        {
            fs::remove_file(path)
        }
        Ok(_) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

/// `now + timeout` as a fallible operation; an unrepresentable deadline is a typed error, never a panic.
fn deadline_after(timeout: Duration, field: &'static str) -> Result<Instant, Error> {
    Instant::now()
        .checked_add(timeout)
        .ok_or(Error::InvalidBudget(field))
}

fn remaining_until(deadline: Instant) -> Option<Duration> {
    let remaining = deadline.saturating_duration_since(Instant::now());
    (!remaining.is_zero()).then_some(remaining)
}

fn is_timeout(e: &io::Error) -> bool {
    matches!(
        e.kind(),
        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::thread;

    static COUNTER: AtomicU32 = AtomicU32::new(0);

    /// Unique synthetic socket directory per test; nothing here is a real supplicant path.
    struct Sandbox {
        dir: PathBuf,
    }

    impl Sandbox {
        fn new(name: &str) -> Self {
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let dir = std::env::temp_dir()
                .join(format!("tetrad-sys-supp-{}-{n}-{name}", std::process::id()));
            fs::create_dir_all(&dir).unwrap();
            Self { dir }
        }

        fn path(&self, name: &str) -> PathBuf {
            self.dir.join(name)
        }

        fn server(&self) -> UnixDatagram {
            let server = UnixDatagram::bind(self.path("remote")).unwrap();
            server
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            server
        }
    }

    impl Drop for Sandbox {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.dir);
        }
    }

    fn budget(timeout_ms: u64) -> Budget {
        Budget {
            reply_capacity: 64,
            event_capacity: 32,
            request_timeout: Duration::from_millis(timeout_ms),
            max_events_per_request: 4,
        }
    }

    /// Receive one command on the fake supplicant and answer with each datagram in order.
    fn answer(server: &UnixDatagram, replies: &[&[u8]]) -> Vec<u8> {
        let mut buf = [0u8; 4096];
        let (len, peer) = server.recv_from(&mut buf).unwrap();
        let peer = peer.as_pathname().unwrap().to_path_buf();
        for reply in replies {
            server.send_to(reply, &peer).unwrap();
        }
        buf[..len].to_vec()
    }

    #[test]
    fn reply_and_events_are_correlated_in_order() {
        let sb = Sandbox::new("order");
        let server = sb.server();
        let mut t = Transport::open(&sb.path("local"), &sb.path("remote"), budget(1000)).unwrap();

        let worker = thread::spawn(move || {
            let cmd = answer(
                &server,
                &[
                    b"<3>CTRL-EVENT-SCAN-STARTED ",
                    b"IFNAME=wlan0 <3>CTRL-EVENT-X",
                    b"OK\n",
                ],
            );
            assert_eq!(cmd, b"STATUS");
            server
        });
        let response = t.request(b"STATUS").unwrap();
        let server = worker.join().unwrap();

        assert_eq!(response.reply.as_bytes(), b"OK\n");
        assert_eq!(response.events.len(), 2);
        assert!(
            matches!(&response.events[0], Receipt::Complete(p) if p.as_bytes().starts_with(b"<3>"))
        );
        assert!(
            matches!(&response.events[1], Receipt::Complete(p) if p.as_bytes().starts_with(b"IFNAME="))
        );
        assert_eq!(format!("{:?}", response.reply), "Payload(3 bytes)");
        assert!(t.poison().is_none());

        // Empty datagram is a reply (stock rule) and does not poison.
        let worker = thread::spawn(move || answer(&server, &[b""]));
        let response = t.request(b"PING").unwrap();
        worker.join().unwrap();
        assert!(response.reply.is_empty());
        assert!(t.poison().is_none());
    }

    #[test]
    fn oversized_event_is_reported_and_oversized_reply_fails_without_poison() {
        let sb = Sandbox::new("oversized");
        let server = sb.server();
        let mut t = Transport::open(&sb.path("local"), &sb.path("remote"), budget(1000)).unwrap();

        let big_event = [b"<3>".as_slice(), &[b'E'; 40]].concat(); // 43 > event_capacity 32
        let worker = thread::spawn(move || {
            answer(&server, &[&big_event, b"OK"]);
            server
        });
        let response = t.request(b"STATUS").unwrap();
        let server = worker.join().unwrap();
        assert_eq!(response.reply.as_bytes(), b"OK");
        assert_eq!(response.events, vec![Receipt::Oversized { at_least: 43 }]);

        // Reply of 70 bytes: capacity 64, buffer 65, so only "at least 65" is known.
        let big_reply = vec![b'R'; 70];
        let worker = thread::spawn(move || {
            answer(&server, &[&big_reply]);
            server
        });
        let err = t.request(b"STATUS").unwrap_err();
        let server = worker.join().unwrap();
        assert!(matches!(err, Error::OversizedReply { at_least: 65 }));
        assert!(t.poison().is_none());

        // Reply between the two capacities (40 bytes) is still a complete reply.
        let mid_reply = vec![b'R'; 40];
        let worker = thread::spawn(move || answer(&server, &[&mid_reply]));
        let response = t.request(b"STATUS").unwrap();
        worker.join().unwrap();
        assert_eq!(response.reply.len(), 40);
        assert!(response.events.is_empty());
    }

    /// Review P1 (2026-10-08): with one-byte capacities the old two-byte buffer turned `IFNAME=...` into an
    /// oversized *reply*, left the real reply queued, and handed it to the next command.
    #[test]
    fn truncated_ifname_event_keeps_its_kind_with_one_byte_capacities() {
        let sb = Sandbox::new("prefix");
        let server = sb.server();
        let mut b = budget(1000);
        b.reply_capacity = 1;
        b.event_capacity = 1;
        assert_eq!(b.receive_buffer_len(), EVENT_PREFIX.len());
        let mut t = Transport::open(&sb.path("local"), &sb.path("remote"), b).unwrap();

        let worker = thread::spawn(move || {
            let cmd = answer(&server, &[b"IFNAME=wlan0 <3>EVENT", b"<", b"A"]);
            assert_eq!(cmd, b"FIRST");
            server
        });
        let response = t.request(b"FIRST").unwrap();
        let server = worker.join().unwrap();
        assert_eq!(response.reply.as_bytes(), b"A");
        assert_eq!(
            response.events,
            vec![
                Receipt::Oversized { at_least: 7 },
                Receipt::Complete(Payload(b"<".to_vec())),
            ]
        );
        assert!(t.poison().is_none());

        // The second command gets its own reply, not a leftover from the first.
        let worker = thread::spawn(move || {
            let cmd = answer(&server, &[b"B"]);
            assert_eq!(cmd, b"SECOND");
            server
        });
        let response = t.request(b"SECOND").unwrap();
        let server = worker.join().unwrap();
        assert_eq!(response.reply.as_bytes(), b"B");
        assert!(response.events.is_empty());

        // A six-byte `IFNAME` without `=` is a reply; above capacity 1 it is an oversized reply, not an event.
        let worker = thread::spawn(move || answer(&server, &[b"IFNAME"]));
        let err = t.request(b"THIRD").unwrap_err();
        worker.join().unwrap();
        assert!(
            matches!(err, Error::OversizedReply { at_least: 6 }),
            "{err:?}"
        );
        assert!(t.poison().is_none());
    }

    #[test]
    fn event_flood_hits_global_deadline_and_poisons() {
        let sb = Sandbox::new("flood-deadline");
        let server = sb.server();
        let mut b = budget(150);
        b.max_events_per_request = MAX_EVENTS_PER_REQUEST;
        let mut t = Transport::open(&sb.path("local"), &sb.path("remote"), b).unwrap();

        let worker = thread::spawn(move || {
            let mut buf = [0u8; 64];
            let (_, peer) = server.recv_from(&mut buf).unwrap();
            let peer = peer.as_pathname().unwrap().to_path_buf();
            let stop = Instant::now() + Duration::from_millis(600);
            while Instant::now() < stop {
                // A dropped client makes send fail; that ends the flood.
                if server.send_to(b"<3>EVENT", &peer).is_err() {
                    break;
                }
                thread::sleep(Duration::from_millis(5));
            }
        });
        let start = Instant::now();
        let err = t.request(b"STATUS").unwrap_err();
        let elapsed = start.elapsed();
        assert!(matches!(err, Error::Timeout), "{err:?}");
        assert!(elapsed >= Duration::from_millis(150));
        // Soft scheduler wall, not an acceptance rule: events must not extend the wait by whole restarts.
        assert!(
            elapsed < Duration::from_millis(500),
            "events must not extend the deadline: {elapsed:?}"
        );
        assert_eq!(t.poison(), Some(PoisonReason::Timeout));
        assert!(matches!(
            t.request(b"STATUS"),
            Err(Error::Poisoned(PoisonReason::Timeout))
        ));
        assert!(matches!(
            t.receive(Duration::from_millis(1)),
            Err(Error::Poisoned(PoisonReason::Timeout))
        ));
        drop(t);
        worker.join().unwrap();
    }

    #[test]
    fn event_budget_poisons_before_deadline() {
        let sb = Sandbox::new("flood-budget");
        let server = sb.server();
        let mut b = budget(2000);
        b.max_events_per_request = 2;
        let mut t = Transport::open(&sb.path("local"), &sb.path("remote"), b).unwrap();

        let worker = thread::spawn(move || {
            answer(&server, &[b"<3>A", b"<3>B", b"<3>C", b"OK"]);
        });
        let start = Instant::now();
        let err = t.request(b"STATUS").unwrap_err();
        worker.join().unwrap();
        assert!(matches!(err, Error::EventFlood));
        assert!(start.elapsed() < Duration::from_millis(1000));
        assert_eq!(t.poison(), Some(PoisonReason::EventFlood));
        // The queued OK must never satisfy a later request.
        assert!(matches!(t.request(b"PING"), Err(Error::Poisoned(_))));
    }

    #[test]
    fn late_reply_cannot_satisfy_next_request_and_fresh_transport_is_clean() {
        let sb = Sandbox::new("late");
        let server = sb.server();
        let mut t = Transport::open(&sb.path("local-1"), &sb.path("remote"), budget(100)).unwrap();

        let worker = thread::spawn(move || {
            let mut buf = [0u8; 64];
            let (_, peer) = server.recv_from(&mut buf).unwrap();
            let peer = peer.as_pathname().unwrap().to_path_buf();
            thread::sleep(Duration::from_millis(250));
            // Late reply to the timed-out request. The client socket still exists, so this is queued.
            server.send_to(b"OK", &peer).unwrap();
            server
        });
        assert!(matches!(t.request(b"STATUS"), Err(Error::Timeout)));
        let server = worker.join().unwrap();
        assert_eq!(t.poison(), Some(PoisonReason::Timeout));
        assert!(matches!(
            t.request(b"PING"),
            Err(Error::Poisoned(PoisonReason::Timeout))
        ));

        // Close discards the queued late reply with the socket and removes the path.
        t.close().unwrap();
        assert!(!sb.path("local-1").exists());

        let mut t = Transport::open(&sb.path("local-2"), &sb.path("remote"), budget(1000)).unwrap();
        let worker = thread::spawn(move || answer(&server, &[b"PONG"]));
        let response = t.request(b"PING").unwrap();
        assert_eq!(worker.join().unwrap(), b"PING");
        assert_eq!(response.reply.as_bytes(), b"PONG");
        assert!(response.events.is_empty());
    }

    /// Review P2 (2026-10-08): a reply whose `recv` completes after the absolute deadline was accepted.
    ///
    /// The peer sends only after `its receipt time + budget`, which is provably later than the transport's
    /// deadline (`call time + budget`, and the call precedes the receipt). So under correct behaviour no
    /// iteration may succeed, whatever the scheduler does; this asserts acceptance, not a wall-clock bound.
    /// The old code accepted most iterations because `SO_RCVTIMEO` rounds up to scheduler ticks.
    #[test]
    fn reply_sent_after_the_deadline_is_never_accepted() {
        let timeout = Duration::from_millis(1);
        for i in 0..20 {
            let sb = Sandbox::new(&format!("cutoff-{i}"));
            let server = sb.server();
            let mut b = budget(1);
            b.request_timeout = timeout;
            let mut t = Transport::open(&sb.path("local"), &sb.path("remote"), b).unwrap();

            let worker = thread::spawn(move || {
                let mut buf = [0u8; 16];
                let (_, peer) = server.recv_from(&mut buf).unwrap();
                let received_at = Instant::now();
                let peer = peer.as_pathname().unwrap().to_path_buf();
                while Instant::now() <= received_at + timeout {
                    thread::sleep(Duration::from_micros(100));
                }
                // Queued if the client socket still exists; it must never be accepted.
                let _ = server.send_to(b"OK", &peer);
            });
            let result = t.request(b"PING");
            worker.join().unwrap();
            assert!(
                matches!(result, Err(Error::Timeout)),
                "iteration {i}: {result:?}"
            );
            assert_eq!(t.poison(), Some(PoisonReason::Timeout));
        }
    }

    #[test]
    fn existing_local_path_is_refused_and_never_unlinked() {
        let sb = Sandbox::new("exists");
        let _server = sb.server();
        let file = sb.path("local");
        fs::write(&file, b"not ours").unwrap();
        assert!(matches!(
            Transport::open(&file, &sb.path("remote"), budget(100)),
            Err(Error::LocalPathExists)
        ));
        assert_eq!(fs::read(&file).unwrap(), b"not ours");

        // A live socket at the path is refused too; stock would unlink it.
        let other = UnixDatagram::bind(sb.path("busy")).unwrap();
        assert!(matches!(
            Transport::open(&sb.path("busy"), &sb.path("remote"), budget(100)),
            Err(Error::LocalPathExists)
        ));
        assert!(sb.path("busy").exists());
        drop(other);
    }

    #[test]
    fn open_failures_and_drop_clean_only_the_owned_socket() {
        let sb = Sandbox::new("cleanup");
        // No remote: connect fails and the just-bound local path is removed.
        let err = Transport::open(&sb.path("local"), &sb.path("missing"), budget(100)).unwrap_err();
        assert!(matches!(err, Error::Connect(_)), "{err:?}");
        assert!(!sb.path("local").exists());

        let _server = sb.server();
        let t = Transport::open(&sb.path("local"), &sb.path("remote"), budget(100)).unwrap();
        assert!(sb.path("local").exists());
        drop(t);
        assert!(!sb.path("local").exists());

        // Explicit close also cleans up and reports success.
        let t = Transport::open(&sb.path("local"), &sb.path("remote"), budget(100)).unwrap();
        t.close().unwrap();
        assert!(!sb.path("local").exists());
    }

    /// Review P2 (2026-10-08): cleanup unlinked any socket node at the path, including a replacement bound by
    /// someone else after ours was removed. Identity is now checked, and `close` consumes cleanup ownership.
    #[test]
    fn replacement_socket_at_the_local_path_is_never_unlinked() {
        let sb = Sandbox::new("replace");
        let server = sb.server();
        let local = sb.path("local");

        // Explicit close: the replacement stays and still works.
        let t = Transport::open(&local, &sb.path("remote"), budget(100)).unwrap();
        fs::remove_file(&local).unwrap();
        let replacement = UnixDatagram::bind(&local).unwrap();
        replacement
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        t.close().unwrap();
        assert!(local.exists());
        server.send_to(b"still mine", &local).unwrap();
        let mut buf = [0u8; 16];
        let len = replacement.recv(&mut buf).unwrap();
        assert_eq!(&buf[..len], b"still mine");
        drop(replacement);
        fs::remove_file(&local).unwrap();

        // Drop takes the same decision.
        let t = Transport::open(&local, &sb.path("remote"), budget(100)).unwrap();
        fs::remove_file(&local).unwrap();
        let replacement = UnixDatagram::bind(&local).unwrap();
        drop(t);
        assert!(local.exists());
        drop(replacement);
        fs::remove_file(&local).unwrap();

        // Cleanup runs once: after it has released the path, a later entry created by someone else is not
        // touched by a second call or by `Drop`.
        let mut t = Transport::open(&local, &sb.path("remote"), budget(100)).unwrap();
        assert!(t.owned_node.is_some());
        t.release_path().unwrap();
        assert!(t.owned_node.is_none());
        assert!(!local.exists());
        let later = UnixDatagram::bind(&local).unwrap();
        t.release_path().unwrap();
        drop(t);
        assert!(local.exists());
        drop(later);
    }

    #[test]
    fn invalid_budget_and_command_are_refused_before_io() {
        let sb = Sandbox::new("invalid");
        let server = sb.server();
        let mut b = budget(100);
        b.reply_capacity = 0;
        assert!(matches!(
            Transport::open(&sb.path("local"), &sb.path("remote"), b),
            Err(Error::InvalidBudget("reply_capacity"))
        ));
        b.reply_capacity = MAX_CAPACITY + 1;
        assert!(matches!(
            Transport::open(&sb.path("local"), &sb.path("remote"), b),
            Err(Error::InvalidBudget("reply_capacity"))
        ));
        b = budget(0);
        assert!(matches!(
            Transport::open(&sb.path("local"), &sb.path("remote"), b),
            Err(Error::InvalidBudget("request_timeout"))
        ));
        b = budget(100);
        b.max_events_per_request = MAX_EVENTS_PER_REQUEST + 1;
        assert!(matches!(
            Transport::open(&sb.path("local"), &sb.path("remote"), b),
            Err(Error::InvalidBudget("max_events_per_request"))
        ));
        assert!(!sb.path("local").exists());

        let mut t = Transport::open(&sb.path("local"), &sb.path("remote"), budget(100)).unwrap();
        for bad in [
            Vec::new(),
            b"STATUS\n".to_vec(),
            b"STA\0TUS".to_vec(),
            vec![b'A'; MAX_COMMAND_BYTES + 1],
        ] {
            assert!(matches!(t.request(&bad), Err(Error::InvalidCommand)));
        }
        assert!(t.poison().is_none());
        // Nothing reached the fake supplicant.
        server
            .set_read_timeout(Some(Duration::from_millis(50)))
            .unwrap();
        let mut buf = [0u8; 16];
        assert!(server.recv_from(&mut buf).is_err());
    }

    /// Review P2 (2026-10-08): `Duration::MAX` passed validation and panicked in `Instant + Duration` inside
    /// `request` and `receive`. Both are now typed refusals, and the deadline addition itself is checked.
    #[test]
    fn extreme_timeouts_are_refused_without_panic() {
        let sb = Sandbox::new("extreme");
        let server = sb.server();
        let mut b = budget(100);
        b.request_timeout = Duration::MAX;
        assert!(matches!(
            Transport::open(&sb.path("local"), &sb.path("remote"), b),
            Err(Error::InvalidBudget("request_timeout"))
        ));
        b.request_timeout = MAX_TIMEOUT + Duration::from_nanos(1);
        assert!(matches!(
            Transport::open(&sb.path("local"), &sb.path("remote"), b),
            Err(Error::InvalidBudget("request_timeout"))
        ));
        b.request_timeout = MAX_TIMEOUT;
        let t = Transport::open(&sb.path("local"), &sb.path("remote"), b).unwrap();
        t.close().unwrap();

        let mut t = Transport::open(&sb.path("local"), &sb.path("remote"), budget(100)).unwrap();
        assert!(matches!(
            t.receive(Duration::MAX),
            Err(Error::InvalidBudget("timeout"))
        ));
        assert!(matches!(
            t.receive(MAX_TIMEOUT + Duration::from_nanos(1)),
            Err(Error::InvalidBudget("timeout"))
        ));
        assert!(t.poison().is_none());
        assert!(matches!(
            deadline_after(Duration::MAX, "timeout"),
            Err(Error::InvalidBudget("timeout"))
        ));

        // Zero wait is an ordinary `None`, and the transport stays usable.
        assert!(t.receive(Duration::ZERO).unwrap().is_none());
        let worker = thread::spawn(move || answer(&server, &[b"PONG"]));
        assert_eq!(t.request(b"PING").unwrap().reply.as_bytes(), b"PONG");
        worker.join().unwrap();
    }

    /// Review P3 (2026-10-08): every retained datagram carried the full receive buffer capacity. Payloads are
    /// now copied out of a reused scratch buffer at their own length.
    #[test]
    fn retained_payloads_are_sized_to_their_datagram() {
        let sb = Sandbox::new("alloc");
        let server = sb.server();
        let mut b = budget(1000);
        b.reply_capacity = MAX_CAPACITY;
        b.event_capacity = 1;
        let mut t = Transport::open(&sb.path("local"), &sb.path("remote"), b).unwrap();
        assert_eq!(t.scratch.len(), MAX_CAPACITY + 1);

        let worker = thread::spawn(move || {
            answer(&server, &[b"<", b"OK"]);
            server
        });
        let response = t.request(b"PING").unwrap();
        let server = worker.join().unwrap();
        let Some(Receipt::Complete(event)) = response.events.into_iter().next() else {
            panic!("expected one complete event");
        };
        let event = event.into_bytes();
        assert_eq!(event, b"<");
        assert_eq!(event.capacity(), 1);
        let reply = response.reply.into_bytes();
        assert_eq!(reply, b"OK");
        assert_eq!(reply.capacity(), 2);

        // The monitor path uses the same copy, and the scratch buffer is reused rather than reallocated.
        let scratch_ptr = t.scratch.as_ptr();
        server.send_to(b"<", sb.path("local")).unwrap();
        let Some(Received::Event(Receipt::Complete(event))) =
            t.receive(Duration::from_millis(500)).unwrap()
        else {
            panic!("expected one complete event");
        };
        assert_eq!(event.into_bytes().capacity(), 1);
        assert_eq!(t.scratch.as_ptr(), scratch_ptr);
        assert_eq!(t.scratch.len(), MAX_CAPACITY + 1);
    }

    #[test]
    fn monitor_attach_then_receive_events_without_poison() {
        let sb = Sandbox::new("monitor");
        let server = sb.server();
        let mut t = Transport::open(&sb.path("mon"), &sb.path("remote"), budget(1000)).unwrap();

        let worker = thread::spawn(move || {
            let cmd = answer(&server, &[b"OK"]);
            assert_eq!(cmd, b"ATTACH");
            server
        });
        t.attach().unwrap();
        let server = worker.join().unwrap();

        // Nothing pending: timeout is an ordinary None, and the zero-wait poll returns at once.
        assert!(t.receive(Duration::from_millis(20)).unwrap().is_none());
        assert!(t.try_receive().unwrap().is_none());
        assert!(t.poison().is_none());

        let peer = sb.path("mon");
        server
            .send_to(b"<3>CTRL-EVENT-SCAN-RESULTS", &peer)
            .unwrap();
        assert!(matches!(
            t.try_receive().unwrap(),
            Some(Received::Event(Receipt::Complete(p))) if p.as_bytes() == b"<3>CTRL-EVENT-SCAN-RESULTS"
        ));
        assert!(t.try_receive().unwrap().is_none());
        server.send_to(b"<3>CTRL-EVENT-CONNECTED", &peer).unwrap();
        server.send_to(&[b'<'; 40], &peer).unwrap();
        server.send_to(b"STRAY", &peer).unwrap();
        assert!(matches!(
            t.receive(Duration::from_millis(500)).unwrap(),
            Some(Received::Event(Receipt::Complete(p))) if p.as_bytes() == b"<3>CTRL-EVENT-CONNECTED"
        ));
        assert!(matches!(
            t.receive(Duration::from_millis(500)).unwrap(),
            Some(Received::Event(Receipt::Oversized { at_least: 40 }))
        ));
        // An uncorrelated non-event is surfaced as-is; the caller decides. Still no poison.
        assert!(matches!(
            t.receive(Duration::from_millis(500)).unwrap(),
            Some(Received::Reply(Receipt::Complete(p))) if p.as_bytes() == b"STRAY"
        ));
        assert!(t.poison().is_none());

        let worker = thread::spawn(move || answer(&server, &[b"FAIL"]));
        assert!(matches!(t.detach(), Err(Error::NotAcknowledged)));
        assert_eq!(worker.join().unwrap(), b"DETACH");
        assert!(t.poison().is_none());
    }

    #[test]
    fn stock_local_path_shape() {
        let p = stock_local_path(Path::new("/tmp"), 7);
        assert_eq!(
            p,
            PathBuf::from(format!("/tmp/wpa_ctrl_{}-7", std::process::id()))
        );
    }
}

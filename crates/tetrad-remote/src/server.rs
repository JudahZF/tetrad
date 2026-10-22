//! The server thread: one connection at a time, each request that needs the device sent to
//! it as a [`Call`] through a channel the device loop drains.

use std::collections::HashMap;
use std::io::{self, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread;
use std::time::{Duration, Instant};

use crate::api::{Call, Reply, Request};
use crate::http::serve_one;

/// How often the accept loop looks at the stop flag.
const ACCEPT_POLL: Duration = Duration::from_millis(100);
/// The longest one connection may take to send its request and read the response.
const CONNECTION_TIME: Duration = Duration::from_secs(10);
/// How long a request waits for the device.
const REPLY_TIME: Duration = Duration::from_secs(5);
/// The wait before a wrong-PIN reply: 10,000 PINs then take hours to try.
const WRONG_PIN_DELAY: Duration = Duration::from_secs(1);

type Message = (Call, Sender<Reply>);

/// A running server. Drop stops it; the thread ends within [`ACCEPT_POLL`] after its open
/// connection.
pub struct Server {
    calls: Receiver<Message>,
    pending: HashMap<u64, Sender<Reply>>,
    stop: Arc<AtomicBool>,
    address: SocketAddr,
}

impl Server {
    pub fn start(address: SocketAddr) -> io::Result<Self> {
        let listener = TcpListener::bind(address)?;
        listener.set_nonblocking(true)?;
        let address = listener.local_addr()?;
        let stop = Arc::new(AtomicBool::new(false));
        let (sender, calls) = mpsc::channel();
        let stopped = Arc::clone(&stop);
        thread::Builder::new()
            .name("remote".into())
            .spawn(move || accept(&listener, &sender, &stopped))?;
        Ok(Self {
            calls,
            pending: HashMap::new(),
            stop,
            address,
        })
    }

    pub fn local_addr(&self) -> SocketAddr {
        self.address
    }

    /// Moves the waiting calls to `out`. Never blocks.
    pub fn drain(&mut self, out: &mut Vec<Call>) {
        for (call, reply) in self.calls.try_iter() {
            self.pending.insert(call.id, reply);
            out.push(call);
        }
    }

    /// Answers the call `id`. A call whose connection gave up is ignored.
    pub fn reply(&mut self, id: u64, reply: Reply) {
        if let Some(sender) = self.pending.remove(&id) {
            let _ = sender.send(reply);
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

fn accept(listener: &TcpListener, calls: &Sender<Message>, stop: &AtomicBool) {
    let mut next_id = 0;
    while !stop.load(Ordering::Relaxed) {
        match listener.accept() {
            Ok((stream, _)) => {
                let _ = connection(stream, calls, &mut next_id);
            }
            Err(_) => thread::sleep(ACCEPT_POLL),
        }
    }
}

fn connection(stream: TcpStream, calls: &Sender<Message>, next_id: &mut u64) -> io::Result<()> {
    stream.set_nonblocking(false)?;
    let mut stream = Deadline {
        stream,
        until: Instant::now() + CONNECTION_TIME,
    };
    serve_one(&mut stream, |request: Request| {
        *next_id += 1;
        let id = *next_id;
        let (sender, reply) = mpsc::channel();
        if calls.send((Call { id, request }, sender)).is_err() {
            return Reply::Off;
        }
        let reply = match reply.recv_timeout(REPLY_TIME) {
            Ok(reply) => reply,
            Err(RecvTimeoutError::Timeout) => Reply::Busy,
            Err(RecvTimeoutError::Disconnected) => Reply::Off,
        };
        if reply == Reply::WrongPin {
            thread::sleep(WRONG_PIN_DELAY);
        }
        reply
    })
}

/// A stream whose reads and writes all end by `until`, so a slow client cannot hold the
/// single connection slot.
struct Deadline {
    stream: TcpStream,
    until: Instant,
}

impl Deadline {
    fn left(&self) -> io::Result<Duration> {
        let left = self.until.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(io::ErrorKind::TimedOut.into());
        }
        Ok(left)
    }
}

impl Read for Deadline {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.stream.set_read_timeout(Some(self.left()?))?;
        self.stream.read(buf)
    }
}

impl Write for Deadline {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.stream.set_write_timeout(Some(self.left()?))?;
        self.stream.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.stream.flush()
    }
}

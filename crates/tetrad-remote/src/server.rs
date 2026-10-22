//! The server thread: one connection at a time, each request that needs the device sent to
//! it as a [`Call`] through a channel the device loop drains.

use std::collections::HashMap;
use std::io::{self, Read, Write};
use std::net::Shutdown;
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::{self, JoinHandle};
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

/// The connection being served, so that a stop can end it.
type Open = Arc<Mutex<Option<TcpStream>>>;

/// A running server. Drop stops it and waits for the thread (at most about [`ACCEPT_POLL`]),
/// so the port is free again when drop returns.
pub struct Server {
    calls: Receiver<Message>,
    pending: HashMap<u64, Sender<Reply>>,
    stop: Arc<AtomicBool>,
    open: Open,
    thread: Option<JoinHandle<()>>,
    address: SocketAddr,
}

impl Server {
    pub fn start(address: SocketAddr) -> io::Result<Self> {
        let listener = TcpListener::bind(address)?;
        listener.set_nonblocking(true)?;
        let address = listener.local_addr()?;
        let stop = Arc::new(AtomicBool::new(false));
        let open = Open::default();
        let (sender, calls) = mpsc::channel();
        let (stopped, served) = (Arc::clone(&stop), Arc::clone(&open));
        let thread = thread::Builder::new()
            .name("remote".into())
            .spawn(move || accept(&listener, &sender, &stopped, &served))?;
        Ok(Self {
            calls,
            pending: HashMap::new(),
            stop,
            open,
            thread: Some(thread),
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
        self.stop.store(true, Ordering::SeqCst);
        // Ends a connection that waits for its client, and with the receiver gone (and every
        // reply sender with it) one that waits for the device.
        let open = self.open.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(stream) = open.as_ref() {
            let _ = stream.shutdown(Shutdown::Both);
        }
        drop(open);
        drop(std::mem::replace(&mut self.calls, mpsc::channel().1));
        self.pending.clear();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn accept(listener: &TcpListener, calls: &Sender<Message>, stop: &AtomicBool, open: &Open) {
    let mut next_id = 0;
    while !stop.load(Ordering::SeqCst) {
        match listener.accept() {
            Ok((stream, _)) => {
                // Published before the stop check: a stop either sees it or is seen here.
                *open.lock().unwrap_or_else(PoisonError::into_inner) = stream.try_clone().ok();
                if !stop.load(Ordering::SeqCst) {
                    let _ = connection(stream, calls, stop, &mut next_id);
                }
                *open.lock().unwrap_or_else(PoisonError::into_inner) = None;
            }
            Err(_) => thread::sleep(ACCEPT_POLL),
        }
    }
}

fn connection(
    stream: TcpStream,
    calls: &Sender<Message>,
    stop: &AtomicBool,
    next_id: &mut u64,
) -> io::Result<()> {
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
            // In steps, so a stop does not wait for the whole delay.
            let until = Instant::now() + WRONG_PIN_DELAY;
            while Instant::now() < until && !stop.load(Ordering::SeqCst) {
                thread::sleep(ACCEPT_POLL);
            }
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

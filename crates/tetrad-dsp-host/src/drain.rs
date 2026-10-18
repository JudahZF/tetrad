//! Receive-ring drain off the main thread, as stock does.
//!
//! Stock (**C**): two uio worker threads empty the receive rings whatever the main thread does.
//! `0x8a4ec4` drains rings 0, 4, 2, 3 and `0x8a4d58` drains ring 1. Each tick waits up to 15 ms on
//! `/dev/uio0`/`/dev/uio1` (`usleep(15000)` when the node did not open), then drains every ring
//! until it is empty, with or without an interrupt. The worker copies each record and gives it to
//! the dispatcher (`0x89d858` SOC1, `0x89cc60` SOC2), which queues it for the main thread
//! (`updateResponses` `0x89ece0`). So a DSP peer never waits more than about 15 ms for a free slot.
//!
//! Here [`SharedRings`] is that hand-off: the message window behind one lock, plus one FIFO of
//! raw records. [`DrainThread`] is the worker. It runs the stock no-node tick (sleep 15 ms, then
//! drain); it does not wait on uio. The session poll drains through the same lock, then takes
//! the FIFO. Records are decoded, and the comms state changes, on the session's thread only.
//!
//! Threading and safety:
//! - One reader per ring: every ring read and its consumer-index release happen under the lock,
//!   so the thread and the session never read the same slot.
//! - Sends use the same lock: the transport stays the one owner of the window (the bring-up tap
//!   keeps per-ring state across calls). A lock hold is one ring operation or one drain pass.
//!   No wait holds it: send and response waits sleep with the lock released.
//! - Order: only a drain under the lock adds to the FIFO, and only the session takes from it,
//!   after a catch-up drain under that same lock. The session therefore decodes records in the
//!   order they left the rings. Order inside a ring is exact. Order across rings is drain order,
//!   as with main-loop polls (stock has none: two threads).
//! - Errors: the thread drops its drain errors. The next session poll drains the same rings,
//!   meets the same error and reports it. Stock has no error path here.
//! - A panic in a backend call poisons the lock. The guarded state is the FIFO and ring
//!   indices that every access reads again from shared memory, so later users go on with it.

use std::collections::VecDeque;
use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use tetrad_dsp_protocol::{CoreId, DspCore};
use zeroize::Zeroizing;

use crate::SharedMemory;
use crate::session::RECEIVE_ORDER;
use crate::transport::{DspTransport, TransportError};

/// Stock uio worker tick: `poll(fd, POLLIN, 15)` or `usleep(15000)` (`0x8a4ec4`, `0x8a4d58`).
pub const DRAIN_PERIOD: Duration = Duration::from_millis(15);

/// One record copied off a ring. Replies carry signing material, the client id and licence
/// data, so the copy is zeroed on drop.
pub(crate) type Record = (CoreId, Zeroizing<Vec<u8>>);

/// The message window and the drained records, shared by a session and its drain thread.
/// Cloning gives another handle on the same state.
pub struct SharedRings<M>(Arc<Mutex<Rings<M>>>);

impl<M> Clone for SharedRings<M> {
    fn clone(&self) -> Self {
        Self(Arc::clone(&self.0))
    }
}

pub(crate) struct Rings<M> {
    pub(crate) transport: DspTransport<M>,
    /// SOC2 rings (2, 3 and Control) are read only after the boot opens them.
    pub(crate) soc2_open: bool,
    inbox: VecDeque<Record>,
}

impl<M: SharedMemory> SharedRings<M> {
    pub fn new(transport: DspTransport<M>) -> Self {
        Self(Arc::new(Mutex::new(Rings {
            transport,
            soc2_open: false,
            inbox: VecDeque::new(),
        })))
    }

    pub(crate) fn lock(&self) -> MutexGuard<'_, Rings<M>> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Moves every ready record of every open ring into the FIFO. Returns the count moved.
    pub fn drain(&self) -> Result<usize, TransportError> {
        self.lock().drain()
    }

    /// Records drained and not yet taken by the session.
    pub fn pending(&self) -> usize {
        self.lock().inbox.len()
    }
}

impl<M: SharedMemory> Rings<M> {
    /// One pass in stock reader order; each ring until it is empty.
    fn drain(&mut self) -> Result<usize, TransportError> {
        let mut count = 0;
        for ring in RECEIVE_ORDER {
            if !self.soc2_open && !matches!(ring, CoreId::Dsp(DspCore::Core0 | DspCore::Core1)) {
                continue;
            }
            while let Some(bytes) = self.transport.try_receive(ring)? {
                self.inbox.push_back((ring, Zeroizing::new(bytes)));
                count += 1;
            }
        }
        Ok(count)
    }

    /// The session's poll: a catch-up drain, then every drained record, oldest first. Records
    /// drained before an error are returned with it.
    pub(crate) fn take(&mut self) -> (VecDeque<Record>, Result<(), TransportError>) {
        let drained = self.drain().map(drop);
        (std::mem::take(&mut self.inbox), drained)
    }
}

/// The stock uio worker pair as one thread. Dropping it stops and joins the thread, so a
/// dropped session never leaves a second reader on the rings (the SOC1 retry opens a new one).
pub struct DrainThread {
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl DrainThread {
    pub fn spawn<M: SharedMemory + Send + 'static>(rings: SharedRings<M>) -> io::Result<Self> {
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = Arc::clone(&stop);
        let worker = thread::Builder::new()
            .name("dsp-drain".into())
            .spawn(move || {
                while !stopped.load(Ordering::Relaxed) {
                    thread::sleep(DRAIN_PERIOD);
                    // The next session poll meets and reports the same error.
                    let _ = rings.drain();
                }
            })?;
        Ok(Self {
            stop,
            worker: Some(worker),
        })
    }
}

impl Drop for DrainThread {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(worker) = self.worker.take() {
            // A panicked worker has nothing left to stop.
            let _ = worker.join();
        }
    }
}

/// Who drains the receive rings between session polls; each platform picks one.
pub trait DrainMode<M> {
    /// Starts the drain for a new session, if this mode has a thread.
    fn start(rings: &SharedRings<M>) -> io::Result<Option<DrainThread>>;
}

/// No thread: only session polls and send waits drain. For windows that cannot leave their
/// thread (the scripted test DSPs, which serve their rings inside clock sleeps).
pub enum MainLoop {}

impl<M> DrainMode<M> for MainLoop {
    fn start(_: &SharedRings<M>) -> io::Result<Option<DrainThread>> {
        Ok(None)
    }
}

/// The stock drain thread ([`DrainThread`]).
pub enum StockThread {}

impl<M: SharedMemory + Send + 'static> DrainMode<M> for StockThread {
    fn start(rings: &SharedRings<M>) -> io::Result<Option<DrainThread>> {
        DrainThread::spawn(rings.clone()).map(Some)
    }
}

#[cfg(test)]
mod tests;

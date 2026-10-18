//! Stock-ordered DSP session: synchronous sends with the `FastDSPCommsLinux` waits (VA `0x54e0ac`),
//! the `DSPCommsARM::sendMessage` crash policy (VA `0x89c6fc`), fragment sends (VA `0x54ed60`),
//! response draining and bounded response waits.
//!
//! Time is injected through [`Clock`]. Nothing here maps memory, touches MMIO
//! or blocks without the clock.

use std::collections::VecDeque;
use std::ops::Deref;
use std::time::{Duration, Instant};

use tetrad_dsp_protocol::fragment::{Fragmenter, MessageMetadata};
pub use tetrad_dsp_protocol::requests::SendPolicy;
use tetrad_dsp_protocol::ring::{CompletionWait, WaitDecision};
use tetrad_dsp_protocol::{CodecLimits, CoreId, DspCore, Error as ProtocolError};

use crate::SharedMemory;
use crate::drain::{DrainMode, DrainThread, SharedRings};
use crate::responses::{
    ALWAYS_ACCEPTED_TYPE, CommsState, CrashCause, Event, IgnoreReason, ReceiveError,
    SendDisposition, SendOutcome,
};
use crate::transport::{ControlPacket, DspTransport, SendReceipt, TransportError};

/// Rings in stock reader order: thread `0x8a4d58` serves ring 1, thread
/// `0x8a4ec4` serves rings 0, 4, 2, 3. One pass visits every ring once.
pub const RECEIVE_ORDER: [CoreId; 5] = [
    CoreId::Dsp(DspCore::Core1),
    CoreId::Dsp(DspCore::Core0),
    CoreId::Control,
    CoreId::Dsp(DspCore::Core2),
    CoreId::Dsp(DspCore::Core3),
];

/// Stock fragment sender (VA `0x54ed60`) waits up to 0.1 s for each slot.
pub const FRAGMENT_SLOT_TIMEOUT_MS: u32 = 100;

/// Poll interval while waiting for responses. Stock waits on Qt signals; this
/// interval is an owned choice that matches the send poll (`usleep(100)`).
pub const RESPONSE_POLL_INTERVAL: Duration = Duration::from_micros(100);

/// Monotonic time and sleep for the stock waits. Tests inject a fake clock.
pub trait Clock {
    /// Time since an arbitrary fixed origin. Must never decrease.
    fn now(&self) -> Duration;
    fn sleep(&mut self, duration: Duration);
}

/// `std` clock for real deployments.
#[derive(Debug, Clone, Copy)]
pub struct SystemClock {
    origin: Instant,
}

impl SystemClock {
    pub fn new() -> Self {
        Self {
            origin: Instant::now(),
        }
    }
}

impl Default for SystemClock {
    fn default() -> Self {
        Self::new()
    }
}

impl Clock for SystemClock {
    fn now(&self) -> Duration {
        self.origin.elapsed()
    }
    fn sleep(&mut self, duration: Duration) {
        std::thread::sleep(duration);
    }
}

/// A sent request. `consumed` is true only after a synchronous wait saw the
/// DSP consumer index reach the slot. `receipt` is `None` only for `0x2d`,
/// which stock reports as sent even when no slot was free (VA `0x89c6fc`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Delivery {
    pub receipt: Option<SendReceipt>,
    pub consumed: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    #[error(transparent)]
    Transport(#[from] TransportError),
    #[error(transparent)]
    Protocol(#[from] ProtocolError),
    /// Not produced by [`DspSession::poll`] since review R4 (malformed
    /// responses are ignored like stock); kept for API compatibility.
    #[error("malformed response on ring {ring:?}: {source}")]
    Response { ring: CoreId, source: ReceiveError },
    #[error("DSP core {0:?} is marked dead")]
    DeadCore(CoreId),
    /// Stock `sendMessage` returned failure. The request may already be
    /// published when only the synchronous wait expired.
    #[error("message {message_type:#04x} to {core:?} timed out ({disposition})")]
    SendTimeout {
        core: CoreId,
        message_type: u8,
        published: bool,
        disposition: SendDisposition,
    },
    #[error("message {message_type:#04x} to {core:?} found no free slot")]
    NoFreeSlot { core: CoreId, message_type: u8 },
    #[error("fragment {index} of message {message_type:#04x} to {core:?} found no free slot")]
    FragmentTimeout {
        core: CoreId,
        message_type: u8,
        index: usize,
    },
    #[error("core {0:?} did not acknowledge the run-list clear; marked dead")]
    RunListTimeout(DspCore),
    /// FitChecker (VA 0x5e62a0): "requested models won't fit in core %d,
    /// skipping deployment!" (stock prints the core + 1).
    #[error("requested models do not fit the {section:?} pool of core {core:?}")]
    WontFit {
        core: DspCore,
        section: qc_format::blob::Section,
    },
    #[error("no {0} response within the wait")]
    ResponseTimeout(&'static str),
    #[error("{0}")]
    Unsupported(&'static str),
    #[error("{0}")]
    Refused(&'static str),
    #[error(transparent)]
    Model(#[from] crate::model::ModelError),
    #[error(transparent)]
    Blob(#[from] crate::blob::BlobError),
    #[error("platform hook failed: {0}")]
    Platform(#[source] Box<dyn std::error::Error + Send + Sync>),
}

/// One owner of the message window plus the comms crash state. Events that a
/// wait does not consume stay queued in arrival order for [`DspSession::next_event`].
/// The window is shared only with this session's drain thread ([`DspSession::start_drain`]).
pub struct DspSession<M, C> {
    rings: SharedRings<M>,
    comms: CommsState,
    clock: C,
    events: VecDeque<(CoreId, Event)>,
    /// Joined on drop, before a later session can open the same rings.
    drain: Option<DrainThread>,
    /// Cores with started models; the next deploy deletes them first.
    deployed: [bool; 4],
    /// [`DspSession::drill_core0_silence`]: the next boot reports core 0 silent.
    drill_core0: bool,
}

impl<M: SharedMemory, C: Clock> DspSession<M, C> {
    /// SOC2 rings (2, 3 and control) stay closed until the boot handshake
    /// opens them or the caller calls [`DspSession::open_soc2`].
    pub fn new(transport: DspTransport<M>, clock: C) -> Self {
        Self {
            rings: SharedRings::new(transport),
            comms: CommsState::new(),
            clock,
            events: VecDeque::new(),
            drain: None,
            deployed: [false; 4],
            drill_core0: false,
        }
    }

    /// Bring-up drill: the next [`DspSession::boot`] fails with [`CORE0_SILENT`] right after
    /// the handshake and the slave boot start, where a silent core 0 fails it on hardware.
    pub fn drill_core0_silence(&mut self) {
        self.drill_core0 = true;
    }

    /// Starts the receive-ring drain that `D` provides (stock uio workers `0x8a4ec4` /
    /// `0x8a4d58`). Stock starts them at SOC1 comms open, before the slave boot.
    pub fn start_drain<D: DrainMode<M>>(&mut self) -> Result<(), SessionError> {
        self.drain = D::start(&self.rings).map_err(|error| SessionError::Platform(error.into()))?;
        Ok(())
    }

    /// The handle a drain thread would get; tests drain through it by hand.
    #[cfg(test)]
    pub(crate) fn rings(&self) -> &SharedRings<M> {
        &self.rings
    }

    /// The transport, locked against the drain thread until the guard drops.
    pub fn transport(&self) -> impl Deref<Target = DspTransport<M>> + '_ {
        TransportGuard(self.rings.lock())
    }
    pub fn comms(&self) -> &CommsState {
        &self.comms
    }
    pub fn clock(&self) -> &C {
        &self.clock
    }
    pub fn clock_mut(&mut self) -> &mut C {
        &mut self.clock
    }
    pub fn is_soc2_open(&self) -> bool {
        self.rings.lock().soc2_open
    }
    /// Stock `OpenSOC2Comms` result (flag `DAT_015630b0`), supplied by the platform.
    pub fn open_soc2(&mut self) {
        self.rings.lock().soc2_open = true;
    }
    /// Stock dead-marking for boot failures (`0x89c848`, `0x89c0bc`).
    pub fn mark_dead(&mut self, cores: &[CoreId]) {
        for core in cores {
            self.comms.mark_dead(*core);
        }
    }

    /// Stock `DSPCommsARM::sendMessage`: refuse dead cores, wait for a slot
    /// up to the timeout, publish, optionally wait for consumption, then apply
    /// the four-strike crash policy. Stock spins for a slot; this polls at
    /// 100 µs (owned deviation).
    pub fn send(
        &mut self,
        packet: ControlPacket<'_>,
        policy: SendPolicy,
    ) -> Result<Delivery, SessionError> {
        let core = packet.destination()?;
        let message_type = packet.message_type();
        if !self.comms.can_send(core) {
            return Err(SessionError::DeadCore(core));
        }
        let published = self.publish_waiting(policy.timeout_ms, |transport| {
            transport.send_control_with_wait_flag(packet, policy.wait)
        })?;
        let consumed = match published {
            Some(receipt) if policy.wait => Some(self.await_consumed(receipt, policy.timeout_ms)?),
            Some(_) => Some(false),
            None => None,
        };
        let outcome = match consumed {
            Some(consumed) if consumed || !policy.wait => SendOutcome::Published,
            _ => SendOutcome::TimedOut,
        };
        let disposition = self.comms.record_send(core, message_type, outcome);
        if disposition == SendDisposition::Crashed {
            self.events
                .push_back((core, Event::Crash(CrashCause::SendTimeouts { core })));
        }
        // Stock returns success for `0x2d` whatever happened (VA 0x89c6fc).
        if outcome == SendOutcome::Published || message_type == ALWAYS_ACCEPTED_TYPE {
            return Ok(Delivery {
                receipt: published,
                consumed: consumed.unwrap_or(false),
            });
        }
        Err(SessionError::SendTimeout {
            core,
            message_type,
            published: published.is_some(),
            disposition,
        })
    }

    /// Stock generic sender (VA `0x51965c`): a payload that fits one fragment
    /// goes through [`DspSession::send`] (wait 0 / 100, crash policy). Longer
    /// payloads go through vtable `+0x38` (VA `0x89b438` → `0x54ed60`): dead
    /// check, up to 100 ms per slot, no wait flag and no crash accounting.
    /// Stock logs a full ring and continues with the next fragment; this stops
    /// at the first one (owned policy) and reports it. Published fragments stay
    /// published.
    pub fn send_generic(
        &mut self,
        metadata: MessageMetadata,
        payload: &[u8],
    ) -> Result<usize, SessionError> {
        let mut fragments = Fragmenter::new(metadata, payload, CodecLimits::default())?.peekable();
        let first = fragments
            .next()
            .expect("a fragmenter yields at least one fragment");
        if fragments.peek().is_none() {
            self.send(ControlPacket::Fragment(first), SendPolicy::new(false, 100))?;
            return Ok(1);
        }
        if !self.comms.can_send(metadata.core) {
            return Err(SessionError::DeadCore(metadata.core));
        }
        let mut count = 0;
        for (index, fragment) in std::iter::once(first).chain(fragments).enumerate() {
            let published = self.publish_waiting(FRAGMENT_SLOT_TIMEOUT_MS, |transport| {
                transport.try_send_fragment(fragment)
            })?;
            if published.is_none() {
                return Err(SessionError::FragmentTimeout {
                    core: metadata.core,
                    message_type: metadata.message_type,
                    index,
                });
            }
            count = index + 1;
        }
        Ok(count)
    }

    /// Drains every open ring once in stock reader order, then decodes every
    /// record drained since the last poll (by this session or its drain
    /// thread) in drain order and queues the events. A malformed response is
    /// acknowledged and queued as [`Event::Ignored`] with
    /// [`IgnoreReason::Malformed`]; stock ignores it. Records drained before a
    /// transport error are queued before the error returns.
    pub fn poll(&mut self) -> Result<usize, SessionError> {
        // Decode outside the lock; later records wait in the FIFO for the next poll.
        let (records, drained) = self.rings.lock().take();
        let mut count = 0;
        for (ring, bytes) in records {
            let events = self.comms.receive(ring, &bytes).unwrap_or_else(|_| {
                vec![Event::Ignored {
                    response_type: bytes.first().copied().unwrap_or(0),
                    reason: IgnoreReason::Malformed,
                }]
            });
            count += events.len();
            self.events
                .extend(events.into_iter().map(|event| (ring, event)));
        }
        drained?;
        Ok(count)
    }

    pub fn next_event(&mut self) -> Option<(CoreId, Event)> {
        self.events.pop_front()
    }

    /// The queued events, oldest first, without removing them.
    pub fn queued_events(&self) -> impl Iterator<Item = &(CoreId, Event)> {
        self.events.iter()
    }

    /// Polls until `select` extracts a value from one queued event, which is
    /// removed. Other events stay queued. `what` names the response in errors.
    pub fn await_event<T>(
        &mut self,
        what: &'static str,
        timeout: Duration,
        mut select: impl FnMut(CoreId, &Event) -> Option<T>,
    ) -> Result<T, SessionError> {
        let started = self.clock.now();
        let mut scanned = 0;
        loop {
            for index in scanned..self.events.len() {
                let (ring, event) = &self.events[index];
                if let Some(value) = select(*ring, event) {
                    self.events.remove(index);
                    return Ok(value);
                }
            }
            scanned = self.events.len();
            if self.clock.now().saturating_sub(started) >= timeout {
                return Err(SessionError::ResponseTimeout(what));
            }
            self.clock.sleep(RESPONSE_POLL_INTERVAL);
            self.poll()?;
        }
    }

    /// Retries `publish` while the ring is full, until the stock timeout.
    /// `None` is the stock "No buffers available" failure.
    fn publish_waiting(
        &mut self,
        timeout_ms: u32,
        mut publish: impl FnMut(&mut DspTransport<M>) -> Result<SendReceipt, TransportError>,
    ) -> Result<Option<SendReceipt>, SessionError> {
        let started = self.clock.now();
        let mut wait = CompletionWait::new(timeout_ms);
        loop {
            // One attempt per lock; the waits below run with the lock released.
            let published = publish(&mut self.rings.lock().transport);
            match published {
                Ok(receipt) => return Ok(Some(receipt)),
                Err(TransportError::RingFull(_)) => {}
                Err(error) => return Err(error.into()),
            }
            match wait.observe(self.clock.now().saturating_sub(started), false)? {
                WaitDecision::Sleep(duration) => {
                    self.poll()?;
                    self.clock.sleep(duration);
                }
                WaitDecision::TimedOut | WaitDecision::Complete => return Ok(None),
            }
        }
    }

    /// Stock synchronous wait: consumer index equals the published slot. Both waits drain the
    /// receive rings into the event queue, also where no drain thread runs: stock reads them on
    /// its own thread, and a peer whose outgoing ring is full stops consuming (S2 on the QC,
    /// 2026-10-15: the Control ring held 14 unread messages through start-up and four `wait`
    /// sends to Control timed out).
    fn await_consumed(
        &mut self,
        receipt: SendReceipt,
        timeout_ms: u32,
    ) -> Result<bool, SessionError> {
        let started = self.clock.now();
        let mut wait = CompletionWait::new(timeout_ms);
        loop {
            let complete = self.rings.lock().transport.is_consumed(receipt)?;
            match wait.observe(self.clock.now().saturating_sub(started), complete)? {
                WaitDecision::Complete => return Ok(true),
                WaitDecision::TimedOut => return Ok(false),
                WaitDecision::Sleep(duration) => {
                    self.poll()?;
                    self.clock.sleep(duration);
                }
            }
        }
    }
}

/// [`DspSession::transport`]: the lock guard, seen as the transport.
struct TransportGuard<'a, M>(std::sync::MutexGuard<'a, crate::drain::Rings<M>>);

impl<M> Deref for TransportGuard<'_, M> {
    type Target = DspTransport<M>;
    fn deref(&self) -> &Self::Target {
        &self.0.transport
    }
}

mod ops;
pub use ops::{
    BlobLoad, BootKeys, BypassUpdate, CORE0_SILENT, CoreLoad, ParameterUpdate, RunSlot,
    SlaveBootOutcome, SlotModel, Soc2Boot, Soc2ImageLengths,
};

#[cfg(test)]
mod tests;

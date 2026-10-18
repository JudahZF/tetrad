//! Logical memory only. Slot fields do not imply stock physical offsets,
//! volatile access, cache coherency, or cross-processor memory barriers.
use crate::{CoreId, Error};
use std::{sync::Arc, time::Duration};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RingKind {
    Outgoing(CoreId),
    Incoming,
}

impl RingKind {
    pub const fn slot_count(self) -> u8 {
        match self {
            Self::Outgoing(CoreId::Dsp(_)) => 10,
            _ => 14,
        }
    }
    pub const fn slot_bytes(self) -> usize {
        match self {
            Self::Outgoing(CoreId::Dsp(_)) => 2048,
            _ => 144,
        }
    }
}

/// Imported/exported simulator state; not a packed hardware memory layout.
/// `Debug` shows only the payload length: a slot can hold key responses.
#[derive(Clone, PartialEq, Eq)]
pub struct SlotImage {
    pub length_le: [u8; 4],
    pub payload: Vec<u8>,
}

impl std::fmt::Debug for SlotImage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SlotImage")
            .field("length_le", &self.length_le)
            .field("payload_bytes", &self.payload.len())
            .finish()
    }
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RingImage {
    pub producer: u8,
    pub consumer: u8,
    pub slots: Vec<SlotImage>,
}

#[derive(Debug, Clone)]
pub struct CompletionToken {
    owner: Arc<()>,
    sequence: u64,
    consumer_index: u8,
}
impl CompletionToken {
    pub fn consumer_index(&self) -> u8 {
        self.consumer_index
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PublicationEvent {
    Payload { slot: u8, bytes: usize },
    Length { slot: u8, length_le: [u8; 4] },
    Producer { index: u8 },
}

#[derive(Debug)]
pub struct Publication {
    pub token: CompletionToken,
    pub log: [PublicationEvent; 3],
}

#[derive(Debug)]
pub struct Ring {
    kind: RingKind,
    image: RingImage,
    owner: Arc<()>,
    published: u64,
    consumed: u64,
}

impl Ring {
    pub fn new(kind: RingKind) -> Self {
        let slots = (0..kind.slot_count())
            .map(|_| SlotImage {
                length_le: [0; 4],
                payload: vec![0; kind.slot_bytes()],
            })
            .collect();
        Self {
            kind,
            image: RingImage {
                producer: 0,
                consumer: 0,
                slots,
            },
            owner: Arc::new(()),
            published: 0,
            consumed: 0,
        }
    }
    pub fn from_image(kind: RingKind, image: RingImage) -> Result<Self, Error> {
        if image.producer >= kind.slot_count() || image.consumer >= kind.slot_count() {
            return Err(Error::Invalid("ring index"));
        }
        if image.slots.len() != usize::from(kind.slot_count()) {
            return Err(Error::Invalid("ring slot count"));
        }
        for slot in &image.slots {
            if slot.payload.len() != kind.slot_bytes() {
                return Err(Error::Invalid("ring slot size"));
            }
            if u32::from_le_bytes(slot.length_le) > kind.slot_bytes() as u32 {
                return Err(Error::Invalid("ring slot length"));
            }
        }
        let queued = (u16::from(image.producer) + u16::from(kind.slot_count())
            - u16::from(image.consumer))
            % u16::from(kind.slot_count());
        Ok(Self {
            kind,
            image,
            owner: Arc::new(()),
            published: u64::from(queued),
            consumed: 0,
        })
    }
    pub fn image(&self) -> &RingImage {
        &self.image
    }
    pub fn is_empty(&self) -> bool {
        self.image.producer == self.image.consumer
    }
    pub fn is_full(&self) -> bool {
        self.next(self.image.producer) == self.image.consumer
    }
    fn next(&self, index: u8) -> u8 {
        (index + 1) % self.kind.slot_count()
    }

    pub fn publish(&mut self, payload: &[u8]) -> Result<Publication, Error> {
        if payload.len() > self.kind.slot_bytes() {
            return Err(Error::Limit("ring payload"));
        }
        if self.is_full() {
            return Err(Error::RingFull);
        }
        let sequence = self
            .published
            .checked_add(1)
            .ok_or(Error::Overflow("ring publication sequence"))?;
        let slot_index = self.image.producer;
        let next_index = self.next(slot_index);
        let slot = &mut self.image.slots[usize::from(slot_index)];
        slot.payload[..payload.len()].copy_from_slice(payload);
        let length_le = (payload.len() as u32).to_le_bytes();
        slot.length_le = length_le;
        self.image.producer = next_index;
        self.published = sequence;
        Ok(Publication {
            token: CompletionToken {
                owner: Arc::clone(&self.owner),
                sequence,
                consumer_index: next_index,
            },
            log: [
                PublicationEvent::Payload {
                    slot: slot_index,
                    bytes: payload.len(),
                },
                PublicationEvent::Length {
                    slot: slot_index,
                    length_le,
                },
                PublicationEvent::Producer { index: next_index },
            ],
        })
    }
    pub fn consume(&mut self) -> Result<Option<Vec<u8>>, Error> {
        if self.is_empty() {
            return Ok(None);
        }
        let sequence = self
            .consumed
            .checked_add(1)
            .ok_or(Error::Overflow("ring consume sequence"))?;
        let slot = &self.image.slots[usize::from(self.image.consumer)];
        let length = u32::from_le_bytes(slot.length_le) as usize;
        let result = slot.payload[..length].to_vec();
        self.image.consumer = self.next(self.image.consumer);
        self.consumed = sequence;
        Ok(Some(result))
    }
    /// Generation-aware simulation: old tokens stay complete after index wrap.
    /// This is stronger bookkeeping than the recovered stock u8 equality wait.
    pub fn is_complete(&self, token: &CompletionToken) -> Result<bool, Error> {
        if !Arc::ptr_eq(&self.owner, &token.owner) {
            return Err(Error::ForeignToken);
        }
        Ok(self.consumed >= token.sequence)
    }
}

pub const COMPLETION_POLL_INTERVAL: Duration = Duration::from_micros(100);
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaitDecision {
    Complete,
    TimedOut,
    Sleep(Duration),
}

/// Caller injects elapsed time and completion observations; no sleeping occurs.
#[derive(Debug)]
pub struct CompletionWait {
    timeout: Duration,
    previous_elapsed: Duration,
}
impl CompletionWait {
    pub fn new(timeout_ms: u32) -> Self {
        Self {
            timeout: Duration::from_millis(u64::from(timeout_ms)),
            previous_elapsed: Duration::ZERO,
        }
    }
    pub fn observe(&mut self, elapsed: Duration, complete: bool) -> Result<WaitDecision, Error> {
        if elapsed < self.previous_elapsed {
            return Err(Error::Invalid("non-monotonic completion clock"));
        }
        self.previous_elapsed = elapsed;
        if complete {
            Ok(WaitDecision::Complete)
        } else if elapsed >= self.timeout {
            Ok(WaitDecision::TimedOut)
        } else {
            Ok(WaitDecision::Sleep(
                COMPLETION_POLL_INTERVAL.min(self.timeout - elapsed),
            ))
        }
    }
}

use crate::{MemoryError, SharedMemory};
use std::cell::RefCell;

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Access {
    Read(usize),
    Write(usize),
    Acquire,
    Publish,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Event {
    Bytes { offset: usize, data: Vec<u8> },
    Word { offset: usize, value: u32 },
    Acquire,
    Barrier,
}

pub(crate) struct FakeMemory {
    pub bytes: Vec<u8>,
    pub events: Vec<Event>,
    pub accesses: RefCell<Vec<Access>>,
    pub fail_write: Option<usize>,
    pub mutate_on_failure: bool,
    pub fail_read_offset: Option<usize>,
    writes: usize,
}

impl FakeMemory {
    pub fn new(bytes: usize) -> Self {
        Self {
            bytes: vec![0; bytes],
            events: Vec::new(),
            accesses: RefCell::new(Vec::new()),
            fail_write: None,
            mutate_on_failure: false,
            fail_read_offset: None,
            writes: 0,
        }
    }

    fn range(&self, offset: usize, len: usize) -> Result<std::ops::Range<usize>, MemoryError> {
        let end = offset
            .checked_add(len)
            .filter(|end| *end <= self.bytes.len())
            .ok_or(MemoryError::OutOfBounds {
                offset,
                len,
                window: self.bytes.len(),
            })?;
        Ok(offset..end)
    }

    fn store(&mut self, offset: usize, bytes: &[u8], event: Event) -> Result<(), MemoryError> {
        let range = self.range(offset, bytes.len())?;
        self.accesses.get_mut().push(Access::Write(offset));
        let fail = self.fail_write == Some(self.writes);
        self.writes += 1;
        self.events.push(event);
        if !fail || self.mutate_on_failure {
            self.bytes[range].copy_from_slice(bytes);
        }
        if fail {
            // Deliberately injected backend error, not an actual alignment defect.
            Err(MemoryError::Misaligned(offset))
        } else {
            Ok(())
        }
    }
}

impl SharedMemory for FakeMemory {
    fn len(&self) -> usize {
        self.bytes.len()
    }

    fn read(&self, offset: usize, buf: &mut [u8]) -> Result<(), MemoryError> {
        self.accesses.borrow_mut().push(Access::Read(offset));
        if self.fail_read_offset == Some(offset) {
            return Err(MemoryError::Misaligned(offset));
        }
        let range = self.range(offset, buf.len())?;
        buf.copy_from_slice(&self.bytes[range]);
        Ok(())
    }

    fn write(&mut self, offset: usize, data: &[u8]) -> Result<(), MemoryError> {
        self.store(
            offset,
            data,
            Event::Bytes {
                offset,
                data: data.to_vec(),
            },
        )
    }

    fn read_u32(&self, offset: usize) -> Result<u32, MemoryError> {
        if !offset.is_multiple_of(4) {
            return Err(MemoryError::Misaligned(offset));
        }
        let mut bytes = [0; 4];
        self.read(offset, &mut bytes)?;
        Ok(u32::from_le_bytes(bytes))
    }

    fn write_u32(&mut self, offset: usize, value: u32) -> Result<(), MemoryError> {
        if !offset.is_multiple_of(4) {
            return Err(MemoryError::Misaligned(offset));
        }
        self.store(offset, &value.to_le_bytes(), Event::Word { offset, value })
    }

    fn acquire_barrier(&mut self) {
        self.accesses.get_mut().push(Access::Acquire);
        self.events.push(Event::Acquire);
    }

    fn publish_barrier(&mut self) {
        self.accesses.get_mut().push(Access::Publish);
        self.events.push(Event::Barrier);
    }
}

/// Window handle shared between a session under test and a scripted DSP peer.
#[derive(Clone)]
pub(crate) struct SharedFake(pub std::rc::Rc<RefCell<FakeMemory>>);

impl SharedFake {
    pub fn new(bytes: usize) -> Self {
        Self(std::rc::Rc::new(RefCell::new(FakeMemory::new(bytes))))
    }
}

impl SharedMemory for SharedFake {
    fn len(&self) -> usize {
        self.0.borrow().len()
    }
    fn read(&self, offset: usize, buf: &mut [u8]) -> Result<(), MemoryError> {
        self.0.borrow().read(offset, buf)
    }
    fn write(&mut self, offset: usize, data: &[u8]) -> Result<(), MemoryError> {
        self.0.borrow_mut().write(offset, data)
    }
    fn read_u32(&self, offset: usize) -> Result<u32, MemoryError> {
        self.0.borrow().read_u32(offset)
    }
    fn write_u32(&mut self, offset: usize, value: u32) -> Result<(), MemoryError> {
        self.0.borrow_mut().write_u32(offset, value)
    }
    fn acquire_barrier(&mut self) {
        self.0.borrow_mut().acquire_barrier();
    }
    fn publish_barrier(&mut self) {
        self.0.borrow_mut().publish_barrier();
    }
}

/// Window handle that may move to another thread (the drain thread tests).
#[derive(Clone)]
pub(crate) struct SyncFake(pub std::sync::Arc<std::sync::Mutex<FakeMemory>>);

impl SyncFake {
    pub fn new(bytes: usize) -> Self {
        Self(std::sync::Arc::new(std::sync::Mutex::new(FakeMemory::new(
            bytes,
        ))))
    }

    pub fn lock(&self) -> std::sync::MutexGuard<'_, FakeMemory> {
        self.0.lock().unwrap()
    }
}

impl SharedMemory for SyncFake {
    fn len(&self) -> usize {
        self.lock().len()
    }
    fn read(&self, offset: usize, buf: &mut [u8]) -> Result<(), MemoryError> {
        self.lock().read(offset, buf)
    }
    fn write(&mut self, offset: usize, data: &[u8]) -> Result<(), MemoryError> {
        self.lock().write(offset, data)
    }
    fn read_u32(&self, offset: usize) -> Result<u32, MemoryError> {
        self.lock().read_u32(offset)
    }
    fn write_u32(&mut self, offset: usize, value: u32) -> Result<(), MemoryError> {
        self.lock().write_u32(offset, value)
    }
    fn acquire_barrier(&mut self) {
        self.lock().acquire_barrier();
    }
    fn publish_barrier(&mut self) {
        self.lock().publish_barrier();
    }
}

/// Publishes one response into an incoming ring, as the DSP would: payload and length first,
/// then the producer index.
pub(crate) fn reply(memory: &mut FakeMemory, ring: tetrad_dsp_protocol::CoreId, bytes: &[u8]) {
    let layout = crate::transport::RingLayout::incoming(ring);
    let slot = (memory.bytes[layout.producer_offset] + 1) % layout.slot_count;
    let payload = layout.payload_offset + usize::from(slot) * layout.slot_bytes;
    memory.bytes[payload..payload + bytes.len()].copy_from_slice(bytes);
    let length = layout.length_offset + usize::from(slot) * 4;
    memory.bytes[length..length + 4].copy_from_slice(&(bytes.len() as u32).to_le_bytes());
    memory.bytes[layout.producer_offset] = slot;
}

/// Unread records in an incoming ring.
pub(crate) fn unread(memory: &FakeMemory, ring: tetrad_dsp_protocol::CoreId) -> u8 {
    let layout = crate::transport::RingLayout::incoming(ring);
    let producer = memory.bytes[layout.producer_offset];
    let consumer = memory.bytes[layout.consumer_offset];
    (producer + layout.slot_count - consumer) % layout.slot_count
}

type Peer = Box<dyn FnMut(&mut FakeMemory, std::time::Duration)>;

/// Fake time. Each sleep advances time, then lets the scripted DSP peer act.
pub(crate) struct FakeClock {
    pub now: std::time::Duration,
    pub window: SharedFake,
    pub peer: Peer,
}

impl FakeClock {
    pub fn new(
        window: SharedFake,
        peer: impl FnMut(&mut FakeMemory, std::time::Duration) + 'static,
    ) -> Self {
        Self {
            now: std::time::Duration::ZERO,
            window,
            peer: Box::new(peer),
        }
    }
}

impl crate::session::Clock for FakeClock {
    fn now(&self) -> std::time::Duration {
        self.now
    }
    fn sleep(&mut self, duration: std::time::Duration) {
        self.now += duration;
        (self.peer)(&mut self.window.0.borrow_mut(), self.now);
    }
}

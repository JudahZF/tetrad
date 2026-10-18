//! Recording fake hardware: sparse physical memory, an access log and a manual clock.
//! A [`Peer`] reacts to each access to emulate the SHARC side of a handshake.
use std::collections::BTreeMap;
use std::ops::Range;
use std::time::Duration;

use crate::booter::{AppMailbox, BooterImage, CoreMap, StreamMailbox};
use crate::exec::{AccessError, CoreControl, PhysicalMemory, word};
use crate::intent::{CoreAction, LoaderCore};
use crate::{FILL_FLAG, FINAL_FLAG, HEADER_BYTES, PhysAddr};

/// Sparse byte-addressed memory; unwritten bytes read as zero.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Memory(BTreeMap<u32, u8>);

impl Memory {
    pub fn read_u32(&self, address: PhysAddr) -> u32 {
        u32::from_le_bytes(std::array::from_fn(|index| {
            self.byte(address.0 + index as u32)
        }))
    }
    pub fn write_u32(&mut self, address: PhysAddr, value: u32) {
        self.write_bytes(address, &value.to_le_bytes());
    }
    pub fn write_bytes(&mut self, address: PhysAddr, bytes: &[u8]) {
        for (index, &byte) in bytes.iter().enumerate() {
            self.0.insert(address.0 + index as u32, byte);
        }
    }
    pub fn bytes(&self, address: PhysAddr, len: usize) -> Vec<u8> {
        (0..len as u32)
            .map(|index| self.byte(address.0 + index))
            .collect()
    }
    fn byte(&self, address: u32) -> u8 {
        self.0.get(&address).copied().unwrap_or(0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    Read32 {
        address: PhysAddr,
        value: u32,
    },
    Write32 {
        address: PhysAddr,
        value: u32,
    },
    Write16 {
        address: PhysAddr,
        value: u16,
    },
    WriteBytes {
        address: PhysAddr,
        len: usize,
    },
    Delay(Duration),
    Core {
        core: LoaderCore,
        action: CoreAction,
    },
}

pub trait Peer {
    /// Called after `access` took effect on `memory`.
    fn on_access(&mut self, memory: &mut Memory, access: &Access);
}

impl Peer for () {
    fn on_access(&mut self, _: &mut Memory, _: &Access) {}
}

impl<F: FnMut(&mut Memory, &Access)> Peer for F {
    fn on_access(&mut self, memory: &mut Memory, access: &Access) {
        self(memory, access);
    }
}

impl<P: Peer> Peer for Vec<P> {
    fn on_access(&mut self, memory: &mut Memory, access: &Access) {
        for peer in self {
            peer.on_access(memory, access);
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Expect {
    Header,
    Payload(u32),
    Finished,
}

/// Emulates the SHARC side of one loadsharc run: the booter requests the LDR stream
/// one 16-byte header at a time, then that block's payload in chunks of at most the
/// buffer capacity, and 0 bytes after FINAL; the kernel then reports start-up and
/// offers `argument_buffer` (a core-local or L2 address). The real booter's request
/// sizes are not recovered; any sizes exercise the same ARM protocol.
#[derive(Debug, Clone)]
pub struct SimSharc {
    pub core: LoaderCore,
    pub stream: StreamMailbox,
    pub app: AppMailbox,
    pub argument_buffer: u32,
    /// Every byte delivered through the stream buffer, in order.
    pub received: Vec<u8>,
    /// Bytes read back from the argument buffer after ARM wrote `0x6666`.
    pub arguments: Option<Vec<u8>>,
    expect: Expect,
    requested: u32,
    argument_len: u32,
}

impl SimSharc {
    pub fn new(booter: &BooterImage<'_>, argument_buffer: u32) -> Self {
        Self {
            core: booter.core,
            stream: booter.stream,
            app: booter.map.app,
            argument_buffer,
            received: Vec::new(),
            arguments: None,
            expect: Expect::Header,
            requested: 0,
            argument_len: 0,
        }
    }
    fn next_request(&self) -> u32 {
        match self.expect {
            Expect::Header => HEADER_BYTES as u32,
            Expect::Payload(left) => left.min(self.stream.capacity),
            Expect::Finished => 0,
        }
    }
    fn consume(&mut self, chunk: Vec<u8>) {
        self.expect = match self.expect {
            Expect::Header => {
                let word = |at: usize| {
                    u32::from_le_bytes([chunk[at], chunk[at + 1], chunk[at + 2], chunk[at + 3]])
                };
                let (flags, count) = (word(0), word(8));
                if flags & FINAL_FLAG != 0 {
                    Expect::Finished
                } else if flags & FILL_FLAG != 0 || count == 0 {
                    Expect::Header
                } else {
                    Expect::Payload(count)
                }
            }
            Expect::Payload(left) => match left - chunk.len() as u32 {
                0 => Expect::Header,
                rest => Expect::Payload(rest),
            },
            Expect::Finished => Expect::Finished,
        };
        self.received.extend(chunk);
    }
}

impl Peer for SimSharc {
    fn on_access(&mut self, memory: &mut Memory, access: &Access) {
        let &Access::Write32 { address, value } = access else {
            return;
        };
        if address == self.stream.arm_to_sharc {
            match value {
                word::ARM_READY => memory.write_u32(self.stream.sharc_to_arm, word::BOOTER_READY),
                word::REQUEST => {
                    self.requested = self.next_request();
                    memory.write_u32(self.stream.to_arm_count, self.requested);
                    memory.write_u32(self.stream.sharc_to_arm, word::GRANT);
                    if self.requested == 0 {
                        memory.write_u32(self.app.sharc_to_arm, word::REQUEST);
                    }
                }
                word::DONE if self.requested != 0 => {
                    let count = memory.read_u32(self.stream.to_sharc_count);
                    self.consume(memory.bytes(self.stream.buffer, count as usize));
                    memory.write_u32(self.stream.sharc_to_arm, word::DONE);
                }
                _ => {}
            }
        } else if address == self.app.arm_to_sharc {
            match value {
                word::REQUEST => {
                    self.argument_len = memory.read_u32(self.app.data_to_sharc);
                    memory.write_u32(self.app.data_to_arm, self.argument_buffer);
                    memory.write_u32(self.app.sharc_to_arm, word::GRANT);
                }
                word::DONE => {
                    if let Some((address, _)) =
                        CoreMap::of(self.core).argument_address(self.argument_buffer)
                    {
                        self.arguments = Some(memory.bytes(address, self.argument_len as usize));
                    }
                }
                _ => {}
            }
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct RecordingBus<P = ()> {
    pub memory: Memory,
    pub log: Vec<Access>,
    pub peer: P,
    /// Simulated time each read costs, so busy waits reach their deadline.
    pub read_cost: Duration,
    /// Mapped physical ranges. Empty means everything is mapped.
    pub windows: Vec<Range<u32>>,
    now: Duration,
}

impl<P: Peer> RecordingBus<P> {
    pub fn new(peer: P) -> Self {
        Self {
            memory: Memory::default(),
            log: Vec::new(),
            peer,
            read_cost: Duration::from_micros(1),
            windows: Vec::new(),
            now: Duration::ZERO,
        }
    }
    fn check(&self, address: PhysAddr, len: usize, align: u32) -> Result<(), AccessError> {
        if !address.0.is_multiple_of(align) {
            return Err(AccessError::Misaligned(address));
        }
        let end = u64::from(address.0) + len as u64;
        let mapped = self.windows.is_empty()
            || self.windows.iter().any(|window| {
                u64::from(window.start) <= u64::from(address.0) && end <= u64::from(window.end)
            });
        if mapped {
            Ok(())
        } else {
            Err(AccessError::Unmapped(address))
        }
    }
    fn record(&mut self, access: Access) {
        self.log.push(access);
        self.peer.on_access(&mut self.memory, &access);
    }
}

impl<P: Peer> PhysicalMemory for RecordingBus<P> {
    fn read_u32(&mut self, address: PhysAddr) -> Result<u32, AccessError> {
        self.check(address, 4, 4)?;
        self.now += self.read_cost;
        let value = self.memory.read_u32(address);
        self.record(Access::Read32 { address, value });
        Ok(value)
    }
    fn write_u32(&mut self, address: PhysAddr, value: u32) -> Result<(), AccessError> {
        self.check(address, 4, 4)?;
        self.memory.write_u32(address, value);
        self.record(Access::Write32 { address, value });
        Ok(())
    }
    fn write_u16(&mut self, address: PhysAddr, value: u16) -> Result<(), AccessError> {
        self.check(address, 2, 2)?;
        self.memory.write_bytes(address, &value.to_le_bytes());
        self.record(Access::Write16 { address, value });
        Ok(())
    }
    fn write_bytes(&mut self, address: PhysAddr, bytes: &[u8]) -> Result<(), AccessError> {
        self.check(address, bytes.len(), 1)?;
        self.memory.write_bytes(address, bytes);
        self.record(Access::WriteBytes {
            address,
            len: bytes.len(),
        });
        Ok(())
    }
    fn now(&self) -> Duration {
        self.now
    }
    fn delay(&mut self, duration: Duration) {
        self.now += duration;
        self.record(Access::Delay(duration));
    }
}

impl<P: Peer> CoreControl for RecordingBus<P> {
    fn control(&mut self, core: LoaderCore, action: CoreAction) -> Result<(), AccessError> {
        self.record(Access::Core { core, action });
        Ok(())
    }
}

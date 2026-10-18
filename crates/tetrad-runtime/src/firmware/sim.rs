//! Offline SC589 model for one owned core: L1 data words, the shared message
//! window, and a register file for SPORT4/DMA10-11/PCG/SEC/TRU/RCU.
//!
//! This is a functional model of the recovered register contract, not of
//! hardware timing. A frame moves one whole descriptor buffer per direction.
//! DMA completion latches SEC source 60 only when the plan enabled it.

use std::collections::BTreeMap;

use tetrad_dsp_protocol::CoreId;
use tetrad_sharc::machine::{AddressUnit, Bus, BusError, MemoryMap};

use super::audio::{
    DMA_STAT_IRQDONE, RCU0_MSG, RECEIVE_SID, SEC0_END, SEC0_GCTL, TRU0_MTR, sec_csid, sec_sctl,
    start_bit,
};
use super::code::RingAddresses;
use super::memory::{
    DATA_BASE, DATA_WORDS, MESSAGE_WINDOW, MESSAGE_WINDOW_BYTES, SharcCore, system_to_data,
};
use crate::resident::audio_port::{PORT_FRAMES, StockPort, TDM_SLOTS};

/// One DMA buffer: `[[word; slots]; frames]`, row = frame.
pub type TdmBlock = [[i32; TDM_SLOTS as usize]; PORT_FRAMES as usize];

const MMIO_START: u32 = 0x3100_0000;
const MMIO_END: u32 = 0x3200_0000;
const PCG0_CTLC0: u32 = 0x310c_b300;
const PCG_ENABLE: u32 = 3 << 30;

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum SimError {
    #[error("audio is not running: {0}")]
    NotRunning(&'static str),
    #[error("descriptor or buffer {0:#x} is outside owned L1 data")]
    Descriptor(u32),
    #[error("a {0}-byte message does not fit one ring slot")]
    MessageSize(usize),
    #[error("the outgoing ring is full")]
    RingFull,
    #[error(transparent)]
    Bus(#[from] BusError),
}

#[derive(Debug, Clone)]
pub struct SimulatedSoc {
    core: SharcCore,
    memory: MemoryMap,
    registers: BTreeMap<u32, u32>,
    writes: Vec<(u32, u32)>,
    pending: Option<u32>,
    active: Option<u32>,
    doorbells: u32,
}

impl SimulatedSoc {
    pub fn new(core: SharcCore) -> Self {
        let mut memory = MemoryMap::default();
        memory
            .add(DATA_BASE, AddressUnit::Word, DATA_WORDS as usize)
            .expect("disjoint regions");
        memory
            .add(
                MESSAGE_WINDOW,
                AddressUnit::Byte,
                (MESSAGE_WINDOW_BYTES / 4) as usize,
            )
            .expect("disjoint regions");
        Self {
            core,
            memory,
            registers: BTreeMap::new(),
            writes: Vec::new(),
            pending: None,
            active: None,
            doorbells: 0,
        }
    }
    /// The ARM's start request (`RCU0_MSG_SET = 1 << (core + 18)`).
    pub fn start(&mut self) {
        *self.registers.entry(RCU0_MSG).or_default() |= start_bit(self.core);
    }
    pub fn memory(&self) -> &MemoryMap {
        &self.memory
    }
    /// Host-side access (for example an ARM view of the message window).
    pub fn memory_mut(&mut self) -> &mut MemoryMap {
        &mut self.memory
    }
    pub fn register(&self, address: u32) -> u32 {
        self.registers.get(&address).copied().unwrap_or(0)
    }
    /// Every MMIO write in program order.
    pub fn mmio_writes(&self) -> &[(u32, u32)] {
        &self.writes
    }
    /// `TRU0_MTR` writes: SHARC -> ARM response notifications.
    pub fn doorbells(&self) -> u32 {
        self.doorbells
    }
    /// The ARM side of one stock message on `ring`'s outgoing ring, in stock publication order:
    /// payload, length, then the producer byte. The resident consumes up to
    /// `code::MESSAGES_PER_FRAME` per frame.
    pub fn publish(&mut self, ring: CoreId, message: &[u8]) -> Result<(), SimError> {
        let ring = RingAddresses::new(ring);
        if message.len() > ring.out_slot_bytes as usize {
            return Err(SimError::MessageSize(message.len()));
        }
        // Index bytes: out producer (ARM), then out consumer (SHARC).
        let indices = self.memory.read_bytes(ring.indices, 2)?;
        let slot = (u32::from(indices[0]) + 1) % ring.out_slots;
        if slot == u32::from(indices[1]) {
            return Err(SimError::RingFull);
        }
        self.memory
            .load_bytes(ring.out_payload + slot * ring.out_slot_bytes, message)?;
        self.memory.load_bytes(
            ring.out_lengths + 4 * slot,
            &(message.len() as u32).to_le_bytes(),
        )?;
        self.memory.load_bytes(ring.indices, &[slot as u8])?;
        Ok(())
    }
    /// A latched, not yet claimed SEC source for this core.
    pub fn interrupt_pending(&self) -> bool {
        self.pending.is_some()
    }

    fn running(&self) -> Result<(), SimError> {
        let enabled = |address: u32, mask: u32| self.register(address) & mask == mask;
        for port in [StockPort::Sport4aReceive, StockPort::Sport4bTransmit] {
            if !enabled(port.dma().base.get() + 8, 1) {
                return Err(SimError::NotRunning("DMA disabled"));
            }
            if !enabled(port.sport().ctl_address.get(), 1) {
                return Err(SimError::NotRunning("SPORT disabled"));
            }
        }
        if !enabled(PCG0_CTLC0, PCG_ENABLE) {
            return Err(SimError::NotRunning("PCG C disabled"));
        }
        Ok(())
    }

    /// Advance one descriptor of `port` and return its buffer's first word.
    fn advance(&mut self, port: StockPort) -> Result<u32, SimError> {
        let next = port.dma().base.get();
        let descriptor = self.register(next);
        let at = system_to_data(self.core, descriptor).ok_or(SimError::Descriptor(descriptor))?;
        let following = self.memory.peek(at.get())?;
        let buffer = self.memory.peek(at.get() + 1)?;
        self.registers.insert(next, following);
        system_to_data(self.core, buffer)
            .map(|word| word.get())
            .ok_or(SimError::Descriptor(buffer))
    }

    /// One frame on both halves: receive `input` into the current receive
    /// buffer, return the current transmit buffer, then latch SID 60.
    pub fn transfer_frame(&mut self, input: &TdmBlock) -> Result<TdmBlock, SimError> {
        self.running()?;
        let rx = self.advance(StockPort::Sport4aReceive)?;
        let tx = self.advance(StockPort::Sport4bTransmit)?;
        let mut output = [[0; TDM_SLOTS as usize]; PORT_FRAMES as usize];
        for (frame, row) in input.iter().enumerate() {
            for (slot, sample) in row.iter().enumerate() {
                let offset = (frame * TDM_SLOTS as usize + slot) as u32;
                self.memory.write(rx + offset, *sample as u32)?;
                output[frame][slot] = self.memory.peek(tx + offset)? as i32;
            }
        }
        let stat = StockPort::Sport4aReceive.dma().stat.get();
        *self.registers.entry(stat).or_default() |= DMA_STAT_IRQDONE;
        let sctl = self.register(sec_sctl(RECEIVE_SID));
        let target = match self.core {
            SharcCore::Core1 => 1,
            SharcCore::Core2 => 2,
        };
        if self.register(SEC0_GCTL) & 1 == 1 && sctl & 0b101 == 0b101 && sctl >> 24 & 15 == target {
            self.pending = Some(RECEIVE_SID);
        }
        Ok(output)
    }
}

impl Bus for SimulatedSoc {
    fn read(&mut self, address: u32) -> Result<u32, BusError> {
        if !(MMIO_START..MMIO_END).contains(&address) {
            return self.memory.read(address);
        }
        if !address.is_multiple_of(4) {
            return Err(BusError::Misaligned(address));
        }
        if address == sec_csid(self.core) {
            // Reading the source ID claims the latched source.
            self.active = self.pending.take();
            return Ok(self.active.unwrap_or(0));
        }
        Ok(self.register(address))
    }

    fn write(&mut self, address: u32, value: u32) -> Result<(), BusError> {
        if !(MMIO_START..MMIO_END).contains(&address) {
            return self.memory.write(address, value);
        }
        if !address.is_multiple_of(4) {
            return Err(BusError::Misaligned(address));
        }
        self.writes.push((address, value));
        match address {
            TRU0_MTR => self.doorbells += 1,
            SEC0_END if self.active == Some(value) => self.active = None,
            _ if address == StockPort::Sport4aReceive.dma().stat.get() => {
                // Write one to clear.
                *self.registers.entry(address).or_default() &= !value;
            }
            _ => {
                self.registers.insert(address, value);
            }
        }
        Ok(())
    }

    fn write_byte(&mut self, address: u32, value: u8) -> Result<(), BusError> {
        if (MMIO_START..MMIO_END).contains(&address) {
            return Err(BusError::Device {
                address,
                reason: "MMIO registers take word accesses",
            });
        }
        self.memory.write_byte(address, value)
    }
}

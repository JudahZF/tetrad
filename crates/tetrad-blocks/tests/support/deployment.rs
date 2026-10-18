//! In-memory V3 peer using only tetrad-dsp-host's public transport/session seams.
//! Its internal test_support module is private to that crate.

use qc_format::blob::{ModuleBlob, Section};
use std::{cell::RefCell, rc::Rc, time::Duration};
use tetrad_dsp_host::blob::{self, PlacedModel};
use tetrad_dsp_host::model::{CoreWindows, ModelMemory};
use tetrad_dsp_host::session::{BlobLoad, Clock, CoreLoad, DspSession, RunSlot};
use tetrad_dsp_host::transport::{DspTransport, MESSAGE_MEMORY_BYTES, RingLayout};
use tetrad_dsp_host::{MemoryError, SharedMemory};
use tetrad_dsp_protocol::{
    CodecLimits,
    fragment::{Fragment, Reassembler},
};
use tetrad_dsp_protocol::{CoreId, DspCore, address::Address};

use tetrad_blocks::{BuiltModule, LinkedModule, OwnedBlock, SectionAddresses};

#[derive(Clone)]
struct Memory(Rc<RefCell<Vec<u8>>>);

impl Memory {
    fn new(bytes: usize) -> Self {
        Self(Rc::new(RefCell::new(vec![0; bytes])))
    }
}

impl SharedMemory for Memory {
    fn len(&self) -> usize {
        self.0.borrow().len()
    }
    fn read(&self, offset: usize, buf: &mut [u8]) -> Result<(), MemoryError> {
        let bytes = self.0.borrow();
        let data = bytes.get(offset..offset.saturating_add(buf.len())).ok_or(
            MemoryError::OutOfBounds {
                offset,
                len: buf.len(),
                window: bytes.len(),
            },
        )?;
        buf.copy_from_slice(data);
        Ok(())
    }
    fn write(&mut self, offset: usize, data: &[u8]) -> Result<(), MemoryError> {
        let mut bytes = self.0.borrow_mut();
        let window = bytes.len();
        let destination = bytes
            .get_mut(offset..offset.saturating_add(data.len()))
            .ok_or(MemoryError::OutOfBounds {
                offset,
                len: data.len(),
                window,
            })?;
        destination.copy_from_slice(data);
        Ok(())
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
        self.write(offset, &value.to_le_bytes())
    }
    fn acquire_barrier(&mut self) {}
    fn publish_barrier(&mut self) {}
}

type Captured = Rc<RefCell<Vec<(CoreId, Vec<u8>)>>>;

struct PeerClock {
    now: Duration,
    memory: Memory,
    sent: Captured,
}

impl Clock for PeerClock {
    fn now(&self) -> Duration {
        self.now
    }
    fn sleep(&mut self, duration: Duration) {
        self.now += duration;
        let mut memory = self.memory.0.borrow_mut();
        for core in [
            DspCore::Core0,
            DspCore::Core1,
            DspCore::Core2,
            DspCore::Core3,
        ] {
            let core = CoreId::Dsp(core);
            let ring = RingLayout::outgoing(core);
            while memory[ring.consumer_offset] != memory[ring.producer_offset] {
                let slot = (memory[ring.consumer_offset] + 1) % ring.slot_count;
                let length = ring.length_offset + usize::from(slot) * 4;
                let length = word(&memory, length) as usize;
                let payload = ring.payload_offset + usize::from(slot) * ring.slot_bytes;
                self.sent
                    .borrow_mut()
                    .push((core, memory[payload..payload + length].to_vec()));
                memory[ring.consumer_offset] = slot;
            }
        }
    }
}

pub fn verify_deployment(module: &ModuleBlob, model_id: u32) -> Vec<PlacedModel> {
    let placements = blob::place(&[Some(module), Some(module)]).unwrap();
    let placed: Vec<_> = placements
        .into_iter()
        .map(|placement| blob::relocate(module, &placement.unwrap()).unwrap())
        .collect();
    assert_ne!(
        placed[0].entry, placed[1].entry,
        "each static-state instance needs its own blob placement"
    );
    let message_memory = Memory::new(MESSAGE_MEMORY_BYTES);
    let sent = Captured::default();
    let clock = PeerClock {
        now: Duration::ZERO,
        memory: message_memory.clone(),
        sent: sent.clone(),
    };
    let mut session = DspSession::new(DspTransport::new(message_memory).unwrap(), clock);
    session.open_soc2();
    let direct =
        |core| ModelMemory::new(core, Address::new(0x24_0000), Memory::new(0x16_0000)).unwrap();
    let mut windows = CoreWindows::new(direct(DspCore::Core0), direct(DspCore::Core1)).unwrap();
    let load = CoreLoad {
        core: DspCore::Core2,
        blobs: (0..2)
            .map(|_| BlobLoad {
                model_id,
                blob: Some(module.clone()),
            })
            .collect(),
        slots: (0..2)
            .map(|index| {
                Some(RunSlot {
                    model_id,
                    blob: Some(index),
                    layout_word: 0,
                    block_word: -1,
                })
            })
            .collect(),
    };
    session.deploy_loads(&mut windows, &[load]).unwrap();
    let sent = sent.borrow();
    let mut reassembler = Reassembler::new(CodecLimits::default());
    let sections: Vec<_> = sent
        .iter()
        .filter(|(core, packet)| *core == CoreId::Dsp(DspCore::Core2) && packet[0] == 7)
        .filter_map(|(_, packet)| {
            reassembler
                .push(Fragment::decode(packet, CodecLimits::default()).unwrap())
                .unwrap()
        })
        .collect();
    let expected: Vec<_> = placed
        .iter()
        .enumerate()
        .flat_map(|(index, model)| {
            model
                .transfers
                .iter()
                .map(move |transfer| (index, transfer))
        })
        .collect();
    assert_eq!(
        sections.len(),
        expected.len(),
        "one reassembled type-7 message per section"
    );
    for (message, (index, transfer)) in sections.into_iter().zip(expected) {
        assert_eq!(message.metadata.slot, index as u8);
        assert_eq!(message.metadata.argument, transfer.address);
        assert_eq!(message.payload, transfer.bytes);
    }
    let start = &sent
        .iter()
        .find(|(core, packet)| *core == CoreId::Dsp(DspCore::Core2) && packet[0] == 0x1c)
        .unwrap()
        .1;
    assert_eq!(start.len(), 16 + 2 * 12);
    for (index, model) in placed.iter().enumerate() {
        assert_eq!(word(start, 16 + index * 12), model.entry);
        assert_eq!(word(start, 20 + index * 12), index as u32);
    }
    placed
}

fn word(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
}

/// V3 for one block: the link-base blob relocated to each `place()` result
/// equals [`LinkedModule::link`] at that placement (all three sections), and
/// two instances deploy through type 7 and `0x1c`. Returns the built module
/// and the placements (instance 0 first).
pub fn verify_blob_path(block: &impl OwnedBlock) -> (BuiltModule, Vec<SectionAddresses>) {
    let built = BuiltModule::build(block).unwrap();
    let mut addresses = Vec::new();
    for placement in blob::place(&[Some(&built.blob), Some(&built.blob)]).unwrap() {
        let placement = placement.unwrap();
        let placed = blob::relocate(&built.blob, &placement).unwrap();
        let at = SectionAddresses {
            dm: placement.remap(Section::Dm.base()),
            visa: placement.remap(Section::Code.base()),
            isa: placement.remap(Section::Isa.base()),
        };
        let linked = LinkedModule::link(block, at).unwrap();
        assert_eq!(placed.entry, at.visa);
        let transfer = |section| {
            &placed
                .transfers
                .iter()
                .find(|transfer| transfer.section == section)
                .unwrap()
                .bytes
        };
        let visa: Vec<u8> = linked.visa.iter().flat_map(|p| p.to_le_bytes()).collect();
        assert_eq!(transfer(Section::Code), &visa);
        assert_eq!(transfer(Section::Isa), &linked.isa.isa48_le_bytes());
        assert_eq!(transfer(Section::Dm), &linked.dm);
        addresses.push(at);
    }
    assert_eq!(verify_deployment(&built.blob, block.model_id()).len(), 2);
    (built, addresses)
}

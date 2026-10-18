//! Recovered byte offsets over injected memory, not a hardware mapping or cache implementation.

use crate::{MemoryError, SharedMemory};
use tetrad_dsp_protocol::{
    CoreId, DspCore, MessageType, ModuleSlot, fragment::Fragment, packet::ParameterChange,
};
use zeroize::Zeroize;

/// ZenUI VA 0x54df98; distinct from the device-tree ICC region.
pub const MESSAGE_MEMORY_BASE: u32 = 0x2008_8000;
pub const MESSAGE_MEMORY_BYTES: usize = 0x1791c;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RingLayout {
    pub producer_offset: usize,
    pub consumer_offset: usize,
    pub payload_offset: usize,
    pub length_offset: usize,
    pub slot_count: u8,
    pub slot_bytes: usize,
    /// Per-slot synchronous-wait flags exist only on these outgoing rings.
    pub wait_flags_offset: Option<usize>,
}

impl RingLayout {
    /// ZenUI VA 0x54e0ac. Indices name the last published/consumed slot.
    pub const fn outgoing(core: CoreId) -> Self {
        let (producer_offset, payload_offset, length_offset, wait_flags_offset) = match core {
            CoreId::Dsp(DspCore::Core0) => (0x784, 0x798, 0x176d8, None),
            CoreId::Dsp(DspCore::Core1) => (0x788, 0x5f78, 0x17738, None),
            CoreId::Dsp(DspCore::Core2) => (0x790, 0xc718, 0x17808, Some(0x178d6)),
            CoreId::Dsp(DspCore::Core3) => (0x794, 0x11ef8, 0x17868, Some(0x178e0)),
            CoreId::Control => (0x78c, 0xb758, 0x17798, Some(0x178c8)),
        };
        Self {
            producer_offset,
            consumer_offset: producer_offset + 1,
            payload_offset,
            length_offset,
            slot_count: if matches!(core, CoreId::Control) {
                14
            } else {
                10
            },
            slot_bytes: if matches!(core, CoreId::Control) {
                144
            } else {
                2048
            },
            wait_flags_offset,
        }
    }

    /// ZenUI VAs 0x54f374 / 0x54f5c0. All incoming rings have fourteen slots.
    pub const fn incoming(core: CoreId) -> Self {
        let (producer_offset, payload_offset, length_offset) = match core {
            CoreId::Dsp(DspCore::Core0) => (0x786, 0x5798, 0x17700),
            CoreId::Dsp(DspCore::Core1) => (0x78a, 0xaf78, 0x17760),
            CoreId::Dsp(DspCore::Core2) => (0x792, 0x11718, 0x17830),
            CoreId::Dsp(DspCore::Core3) => (0x796, 0x16ef8, 0x17890),
            CoreId::Control => (0x78e, 0xbf38, 0x177d0),
        };
        Self {
            producer_offset,
            consumer_offset: producer_offset + 1,
            payload_offset,
            length_offset,
            slot_count: 14,
            slot_bytes: 144,
            wait_flags_offset: None,
        }
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum TransportError {
    #[error(transparent)]
    Memory(#[from] MemoryError),
    #[error(transparent)]
    Packet(#[from] tetrad_dsp_protocol::Error),
    #[error("transport is faulted after an ambiguous backend write; rebind only after quiescence")]
    Faulted,
    #[error("ring index {value} at offset {offset:#x} exceeds slot count {slot_count}")]
    InvalidIndex {
        offset: usize,
        value: u8,
        slot_count: u8,
    },
    #[error("ring is full for core {0:?}")]
    RingFull(CoreId),
    #[error("payload length {bytes} exceeds slot capacity {capacity}")]
    PayloadTooLarge { bytes: usize, capacity: usize },
    #[error("incoming slot {slot} declares {bytes} bytes, exceeding capacity {capacity}")]
    InvalidLength {
        slot: u8,
        bytes: u32,
        capacity: usize,
    },
    #[error("message type {0} requires a separate path; generic ring fallback is not supported")]
    SpecialMessageType(u8),
    #[error("send receipt is invalid or a later publication superseded it")]
    ReceiptSuperseded,
}

/// Buffers per module slot in the receive-buffer table (VA `0x54e710` bound).
pub const RECEIVE_BUFFERS_PER_SLOT: u8 = 4;

/// One DSP-published receive buffer. Address 0 means none is published.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReceiveBuffer {
    pub address: u32,
    pub bytes: u32,
}

/// Publication only, not DSP acknowledgement or generation-safe completion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SendReceipt {
    pub core: CoreId,
    pub slot: u8,
    pub byte_length: usize,
}

/// Complete, validated request packets. These already have their stock header;
/// wrapping them in a generic fragment would change their wire layout.
#[derive(Debug, Clone, Copy)]
pub enum ControlPacket<'a> {
    TunerLooper(&'a tetrad_dsp_protocol::tuner_looper::RequestFrame),
    Controls(&'a tetrad_dsp_protocol::controls::RequestFrame),
    Hid(&'a tetrad_dsp_protocol::hid::HidReportFrame),
    Diagnostics(&'a tetrad_dsp_protocol::synchronization::DiagnosticsRequestFrame),
    MetronomeBlock(&'a tetrad_dsp_protocol::controls::MetronomeParameterBlock),
    Request(&'a tetrad_dsp_protocol::requests::RequestFrame),
    Bulk(&'a tetrad_dsp_protocol::requests::BulkFrame),
    Parameter(&'a ParameterChange),
    /// One generic fragment sent through `sendMessage` (single-fragment path).
    Fragment(Fragment<'a>),
    /// `0x12`/`0x13`/`0x14` to core 0; tetrad-loader owns the layout.
    SlaveBoot(tetrad_loader::soc2::SlaveBootRequest),
}

impl ControlPacket<'_> {
    /// Recovered destination; also the stock `sendMessage` core argument.
    pub fn destination(&self) -> Result<CoreId, tetrad_dsp_protocol::Error> {
        match self {
            Self::TunerLooper(frame) => CoreId::try_from(frame.request().destination()),
            Self::Controls(frame) => CoreId::try_from(frame.request().destination()),
            Self::Hid(_) | Self::MetronomeBlock(_) => Ok(CoreId::Control),
            Self::Diagnostics(frame) => Ok(CoreId::Dsp(frame.core())),
            Self::Request(frame) => Ok(frame.request().destination()),
            Self::Bulk(frame) => Ok(frame.destination()),
            Self::Parameter(packet) => Ok(CoreId::Dsp(packet.target().core)),
            Self::Fragment(fragment) => Ok(fragment.metadata().core),
            Self::SlaveBoot(_) => Ok(CoreId::Dsp(DspCore::Core0)),
        }
    }

    pub fn message_type(&self) -> u8 {
        match self {
            Self::TunerLooper(frame) => frame.as_bytes()[0],
            Self::Controls(frame) => frame.as_bytes()[0],
            Self::Hid(frame) => frame.as_bytes()[0],
            Self::Diagnostics(frame) => frame.as_bytes()[0],
            Self::MetronomeBlock(_) => MessageType::MetronomeDestination as u8,
            Self::Request(frame) => frame.as_bytes()[0],
            Self::Bulk(frame) => frame.as_bytes()[0],
            Self::Parameter(packet) => packet.as_bytes()[0],
            Self::Fragment(fragment) => fragment.metadata().message_type,
            Self::SlaveBoot(request) => request.kind as u8,
        }
    }
}

/// One host owner for all rings in a pre-existing window. Construction never clears it.
///
/// The caller must exclude other host publishers/consumers and satisfy the
/// `SharedMemory` coherency and ordering contract. No physical backend is supplied.
/// No method sleeps, polls a device, or accesses MMIO itself.
pub struct DspTransport<M> {
    memory: M,
    faulted: bool,
}

impl<M: SharedMemory> DspTransport<M> {
    pub fn new(memory: M) -> Result<Self, TransportError> {
        if memory.len() < MESSAGE_MEMORY_BYTES {
            return Err(MemoryError::OutOfBounds {
                offset: 0,
                len: MESSAGE_MEMORY_BYTES,
                window: memory.len(),
            }
            .into());
        }
        Ok(Self {
            memory,
            faulted: false,
        })
    }

    pub fn memory(&self) -> &M {
        &self.memory
    }

    /// Returns the backend without clearing memory or acknowledging queued messages.
    pub fn into_memory(self) -> M {
        self.memory
    }

    pub fn is_faulted(&self) -> bool {
        self.faulted
    }

    /// Preserves caller-owned opaque packet bytes and all float bits.
    pub fn send_parameter(
        &mut self,
        packet: &ParameterChange,
    ) -> Result<SendReceipt, TransportError> {
        self.publish(CoreId::Dsp(packet.target().core), packet.as_bytes(), false)
    }

    /// Publishes the exact packet to its recovered destination. Ring-full is
    /// a refusal before writes; a backend write failure faults this transport.
    pub fn send_control(
        &mut self,
        packet: ControlPacket<'_>,
    ) -> Result<SendReceipt, TransportError> {
        self.send_control_with_wait_flag(packet, false)
    }

    /// Publishes with the recovered synchronous flag for cores 2/3/4. This
    /// never waits: the owner retains the receipt and checks peer consumption.
    /// Cores 0/1 have no flag, but stock also waits on their consumer index.
    pub fn send_control_with_wait_flag(
        &mut self,
        packet: ControlPacket<'_>,
        wait: bool,
    ) -> Result<SendReceipt, TransportError> {
        let destination = packet.destination()?;
        let bytes = match packet {
            ControlPacket::TunerLooper(frame) => frame.as_bytes(),
            ControlPacket::Controls(frame) => frame.as_bytes(),
            ControlPacket::Hid(frame) => frame.as_bytes().as_slice(),
            ControlPacket::Diagnostics(frame) => frame.as_bytes().as_slice(),
            ControlPacket::Request(frame) => frame.as_bytes(),
            ControlPacket::Bulk(frame) => frame.as_bytes(),
            ControlPacket::Parameter(packet) => packet.as_bytes().as_slice(),
            ControlPacket::MetronomeBlock(block) => {
                return self.publish(destination, &block.to_frame(), wait);
            }
            ControlPacket::Fragment(fragment) => {
                refuse_direct_types(fragment)?;
                return self.publish(destination, &fragment.to_bytes(), wait);
            }
            ControlPacket::SlaveBoot(request) => {
                return self.publish(destination, &request.encode(), wait);
            }
        };
        self.publish(destination, bytes, wait)
    }

    /// Stock `0x54e0ac` compares the consumer index to the published slot for
    /// every core; it does not wait for the per-slot flag to clear. Valid only
    /// while this exclusive publisher has made no later send on that core and
    /// the peer cannot reuse the slot. This is not a generation-safe receipt.
    pub fn is_consumed(&mut self, receipt: SendReceipt) -> Result<bool, TransportError> {
        self.ensure_ready()?;
        let layout = RingLayout::outgoing(receipt.core);
        if receipt.slot >= layout.slot_count || receipt.byte_length > layout.slot_bytes {
            return Err(TransportError::ReceiptSuperseded);
        }
        let (producer, consumer) = self.indices(layout)?;
        if producer != receipt.slot {
            return Err(TransportError::ReceiptSuperseded);
        }
        self.memory.acquire_barrier();
        Ok(consumer == receipt.slot)
    }

    /// Sends one validated generic fragment. On full, retain and retry this fragment.
    /// Previously published fragments are not rolled back if a later send fails.
    pub fn try_send_fragment(
        &mut self,
        fragment: Fragment<'_>,
    ) -> Result<SendReceipt, TransportError> {
        refuse_direct_types(fragment)?;
        self.publish(fragment.metadata().core, &fragment.to_bytes(), false)
    }

    /// Copies the next incoming slot before releasing it. An empty ring or a
    /// zero-length slot returns `None` without acknowledgement; malformed lengths
    /// are retained. Lengths are not cleared on consumption, so zero is not a
    /// generation-safe readiness marker after ring reuse. The DSP must publish
    /// payload and length before its producer index with coherent release ordering.
    pub fn try_receive(&mut self, core: CoreId) -> Result<Option<Vec<u8>>, TransportError> {
        self.ensure_ready()?;
        let layout = RingLayout::incoming(core);
        let (producer, consumer) = self.indices(layout)?;
        if producer == consumer {
            return Ok(None);
        }
        self.memory.acquire_barrier();
        let slot = (consumer + 1) % layout.slot_count;
        let bytes = self
            .memory
            .read_u32(layout.length_offset + usize::from(slot) * 4)?;
        // Stock receive loops (VAs 0x8a4d58 / 0x8a4ec4) stop without ack on zero.
        if bytes == 0 {
            return Ok(None);
        }
        if bytes > layout.slot_bytes as u32 {
            return Err(TransportError::InvalidLength {
                slot,
                bytes,
                capacity: layout.slot_bytes,
            });
        }
        self.memory.acquire_barrier();
        let mut result = vec![0; bytes as usize];
        self.memory.read(
            layout.payload_offset + usize::from(slot) * layout.slot_bytes,
            &mut result,
        )?;
        self.memory.publish_barrier();
        if let Err(error) = self.memory.write(layout.consumer_offset, &[slot]) {
            self.faulted = true;
            result.zeroize();
            return Err(error.into());
        }
        Ok(Some(result))
    }

    /// Reads the DSP-published receive-buffer table (VA `0x54e710`): core 0
    /// addresses at `+0x4`, sizes at `+0x1e4`; core 1 at `+0x3c4` / `+0x5a4`;
    /// each `[30 slots][4 buffers]` of LE u32. Cores 2/3 have no table.
    pub fn receive_buffer(
        &mut self,
        core: DspCore,
        slot: ModuleSlot,
        index: u8,
    ) -> Result<Option<ReceiveBuffer>, TransportError> {
        self.ensure_ready()?;
        let (addresses, sizes) = match core {
            DspCore::Core0 => (0x4, 0x1e4),
            DspCore::Core1 => (0x3c4, 0x5a4),
            DspCore::Core2 | DspCore::Core3 => return Ok(None),
        };
        if index >= RECEIVE_BUFFERS_PER_SLOT {
            return Ok(None);
        }
        let entry = (usize::from(slot.index()) * usize::from(RECEIVE_BUFFERS_PER_SLOT)
            + usize::from(index))
            * 4;
        let address = self.memory.read_u32(addresses + entry)?;
        let bytes = self.memory.read_u32(sizes + entry)?;
        self.memory.acquire_barrier();
        Ok(Some(ReceiveBuffer { address, bytes }))
    }

    fn ensure_ready(&self) -> Result<(), TransportError> {
        if self.faulted {
            Err(TransportError::Faulted)
        } else {
            Ok(())
        }
    }

    fn indices(&self, layout: RingLayout) -> Result<(u8, u8), TransportError> {
        let mut indices = [0; 2];
        self.memory.read(layout.producer_offset, &mut indices)?;
        for (index, value) in indices.iter().copied().enumerate() {
            if value >= layout.slot_count {
                return Err(TransportError::InvalidIndex {
                    offset: layout.producer_offset + index,
                    value,
                    slot_count: layout.slot_count,
                });
            }
        }
        Ok((indices[0], indices[1]))
    }

    fn publish(
        &mut self,
        core: CoreId,
        bytes: &[u8],
        wait: bool,
    ) -> Result<SendReceipt, TransportError> {
        self.ensure_ready()?;
        let layout = RingLayout::outgoing(core);
        if bytes.len() > layout.slot_bytes {
            return Err(TransportError::PayloadTooLarge {
                bytes: bytes.len(),
                capacity: layout.slot_bytes,
            });
        }
        let (producer, consumer) = self.indices(layout)?;
        let slot = (producer + 1) % layout.slot_count;
        if slot == consumer {
            return Err(TransportError::RingFull(core));
        }
        // Observe peer consumption before reusing its slot.
        self.memory.acquire_barrier();
        // VA 0x54e0ac uses NEXT slot, unlike tetrad-dsp-protocol's logical Ring.
        let result = (|| {
            self.memory.write(
                layout.payload_offset + usize::from(slot) * layout.slot_bytes,
                bytes,
            )?;
            self.memory.publish_barrier();
            self.memory.write_u32(
                layout.length_offset + usize::from(slot) * 4,
                bytes.len() as u32,
            )?;
            if let Some(offset) = layout.wait_flags_offset {
                self.memory
                    .write(offset + usize::from(slot), &[u8::from(wait)])?;
            }
            self.memory.publish_barrier();
            self.memory.write(layout.producer_offset, &[slot])
        })();
        if let Err(error) = result {
            // Err may follow a partial write, including producer publication.
            self.faulted = true;
            return Err(error.into());
        }
        Ok(SendReceipt {
            core,
            slot,
            byte_length: bytes.len(),
        })
    }
}

/// VA 0x51965c copies LoadModel and ReceiveData straight into core 0/1
/// memory; only cores 2..=4 take them as generic fragments.
fn refuse_direct_types(fragment: Fragment<'_>) -> Result<(), TransportError> {
    let metadata = fragment.metadata();
    if matches!(
        MessageType::try_from(metadata.message_type),
        Ok(MessageType::LoadModel | MessageType::ReceiveData)
    ) && matches!(metadata.core, CoreId::Dsp(DspCore::Core0 | DspCore::Core1))
    {
        return Err(TransportError::SpecialMessageType(metadata.message_type));
    }
    Ok(())
}

#[cfg(test)]
mod tests;

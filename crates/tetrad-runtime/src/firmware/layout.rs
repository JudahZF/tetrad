//! Owned resident data layout in L1 block 1 (normal-word addresses).
//!
//! Everything the generated code reads is a table here, so a new graph needs
//! new data, not new code. Word offsets below are the owned resident ABI.

use std::collections::BTreeMap;

use super::code::BLOCK_STACK_WORDS;
use super::graph::{FramePlan, SLOT_COUNT, SlotId};
use super::memory::{DATA_BASE, DATA_WORDS, PlacementError, SharcCore, data_system_address};
use crate::resident::audio_port::{PORT_FRAMES, TDM_SLOTS};
use tetrad_sharc::address::DmWordAddress32;

pub const FRAMES: u32 = PORT_FRAMES;
/// Owned parameter words per module slot.
pub const PARAMETERS_PER_SLOT: u32 = 16;
/// One handler word per type byte, so any byte dispatches without a range check.
pub const HANDLER_ENTRIES: u32 = 256;
const DMA_WORDS: u32 = TDM_SLOTS * PORT_FRAMES;

/// Control block word offsets.
pub mod control {
    pub const FRAME_COUNT: u32 = 0;
    /// EMUCLK cycles of the last frame (owned CPU accounting).
    pub const CYCLES_LAST: u32 = 1;
    pub const CYCLES_PEAK: u32 = 2;
    pub const FRAME_START: u32 = 3;
    /// Ring messages consumed.
    pub const MESSAGES: u32 = 4;
    /// Messages consumed without an owned handler.
    pub const UNSUPPORTED: u32 = 5;
    pub const LAST_UNSUPPORTED: u32 = 6;
    /// Messages with a bad slot, index, value or size.
    pub const REJECTED: u32 = 7;
    /// Responses not sent because the incoming ring was full.
    pub const DROPPED_RESPONSES: u32 = 8;
    /// Receive completions mod 2: the DMA buffer that just completed.
    pub const RX_PARITY: u32 = 9;
    /// Private copies of the indices this core owns.
    pub const OUT_CONSUMER: u32 = 10;
    pub const IN_PRODUCER: u32 = 11;
    /// Vector number of the last fault interrupt (0 = none).
    pub const FAULT: u32 = 12;
    /// SEC sources claimed that were not the receive DMA.
    pub const STRAY_SOURCES: u32 = 13;
    pub const WORDS: u32 = 16;
}

/// Node record word offsets. A record with kernel 0 ends the table.
pub mod node {
    pub const KERNEL: u32 = 0;
    pub const INPUT0: u32 = 1;
    pub const INPUT1: u32 = 2;
    pub const OUTPUT: u32 = 3;
    pub const PARAMETERS: u32 = 4;
    pub const FLAGS: u32 = 5;
    /// Output peak |x| as f32 bits since the last meter read.
    pub const PEAK: u32 = 6;
    pub const CYCLES: u32 = 7;
    /// Stock module id BulkStartModel must name to start this node.
    pub const MODULE: u32 = 8;
    pub const WORDS: u32 = 9;
    pub const ACTIVE: u32 = 1;
    pub const BYPASS: u32 = 2;
    /// Set when the kernel has one input, so bypass can copy it.
    pub const BYPASSABLE: u32 = 4;
    /// Set when `MODULE` holds an id; without it no BulkStartModel starts the node.
    pub const MODULE_KNOWN: u32 = 8;
    /// Set by an owned block kernel (adapter B) after its reset. DeleteAllModels and
    /// BulkStartModel clear it, so the next call resets the block and resends every
    /// parameter word (review-w4-dsp F7). Stock-shaped kernels ignore it.
    pub const STARTED: u32 = 16;
    /// With `BYPASS`: bypass with mute (ModelBypass 2.0). Stock ramps both
    /// mixer gains to 0 (k1 `0x1659b0`); built-in kernels write silence.
    pub const MUTE: u32 = 32;
    /// The kernel runs the bypass mixer itself (owned blocks, adapter B), so
    /// the scheduler calls it also while `BYPASS` is set.
    pub const SELF_BYPASS: u32 = 64;
}

/// Response staging: 8-byte header plus the largest owned body (76 bytes).
pub const RESPONSE_WORDS: u32 = 21;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DataLayout {
    pub control: u32,
    pub nodes: u32,
    pub slot_index: u32,
    pub parameters: u32,
    /// `[count, (rx word offset, buffer)...]` and the transmit counterpart.
    pub inputs: u32,
    pub outputs: u32,
    pub message_handlers: u32,
    pub response: u32,
    pub rx: [u32; 2],
    pub tx: [u32; 2],
    pub rx_descriptors: [u32; 2],
    pub tx_descriptors: [u32; 2],
    /// First word of each owned block's `code::block_memory` image. Placed
    /// before the DMA buffers, so the initialized data literal covers it.
    pub node_memory: BTreeMap<SlotId, u32>,
    pub buffers: u32,
    /// Top word of the adapter-B stack (`BLOCK_STACK_WORDS` words, the last
    /// region); `None` without owned blocks.
    pub stack_top: Option<u32>,
    pub end: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum LayoutError {
    #[error("resident data needs {0} words, more than block 1 provides")]
    Size(u32),
    #[error(transparent)]
    Placement(#[from] PlacementError),
}

impl DataLayout {
    /// `node_memory` gives the memory words of each owned block node.
    pub fn new<K>(
        plan: &FramePlan<K>,
        node_memory: impl IntoIterator<Item = (SlotId, u32)>,
    ) -> Result<Self, LayoutError> {
        let mut next = DATA_BASE;
        let mut take = |words: u32| {
            let start = next;
            // Saturates into the size error below.
            next = next.saturating_add(words);
            start
        };
        let control = take(control::WORDS);
        let nodes = take(node::WORDS * (plan.steps.len() as u32 + 1));
        let slot_index = take(u32::from(SLOT_COUNT));
        let parameters = take(u32::from(SLOT_COUNT) * PARAMETERS_PER_SLOT);
        let inputs = take(1 + 2 * plan.inputs.len() as u32);
        let outputs = take(1 + 2 * plan.outputs.len() as u32);
        let message_handlers = take(HANDLER_ENTRIES);
        let response = take(RESPONSE_WORDS);
        let rx_descriptors = [take(2), take(2)];
        let tx_descriptors = [take(2), take(2)];
        let node_memory: BTreeMap<SlotId, u32> = node_memory
            .into_iter()
            .map(|(slot, words)| (slot, take(words)))
            .collect();
        let rx = [take(DMA_WORDS), take(DMA_WORDS)];
        let tx = [take(DMA_WORDS), take(DMA_WORDS)];
        let buffers = take(FRAMES * u32::from(plan.buffer_count));
        let stack_top =
            (!node_memory.is_empty()).then(|| take(BLOCK_STACK_WORDS) + BLOCK_STACK_WORDS - 1);
        let end = next;
        if end - DATA_BASE > DATA_WORDS {
            return Err(LayoutError::Size(end - DATA_BASE));
        }
        Ok(Self {
            control,
            nodes,
            slot_index,
            parameters,
            inputs,
            outputs,
            message_handlers,
            response,
            rx,
            tx,
            rx_descriptors,
            tx_descriptors,
            node_memory,
            buffers,
            stack_top,
            end,
        })
    }

    pub fn buffer(&self, index: u16) -> u32 {
        self.buffers + FRAMES * u32::from(index)
    }
    pub fn slot_parameters(&self, slot: u8) -> u32 {
        self.parameters + PARAMETERS_PER_SLOT * u32::from(slot)
    }
    /// System byte address of a data word, for DMA descriptors.
    pub fn system(&self, core: SharcCore, word: u32) -> Result<u32, LayoutError> {
        Ok(data_system_address(core, DmWordAddress32::new(word))?)
    }
}

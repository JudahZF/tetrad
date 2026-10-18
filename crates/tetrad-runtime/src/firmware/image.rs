//! Compose a complete owned resident: graph plan, generated code, initialized
//! data tables, and the LDR stream `tetrad-loader` parses. Path (b) images also
//! place owned tetrad-blocks bodies as node kernels (adapter B, `code.rs`).

use std::collections::BTreeMap;

use tetrad_blocks::OwnedBlock;
use tetrad_dsp_protocol::{CoreId, DspCore};
use tetrad_sharc::address::PmWordAddress24;
use tetrad_sharc::emit::{Label, OwnedCode};
use tetrad_sharc::ldr::{LdrBuilder, LdrError, LdrImage};
use tetrad_sharc::machine::{Bus, BusError, ProgramError, ProgramMemory};

use super::audio::bring_up;
use super::code::{
    BlockCode, BlockKernelError, BlockNode, CodeConfig, CodeGenerationError, HANDLED, INIT,
    KERNEL_GAIN, KERNEL_MIX, NOT_DELIVERED, UNSUPPORTED_HANDLER, block_memory, emit_block_kernels,
    generate,
};
use super::graph::{FramePlan, GraphError, GraphSpec, Kernel, KernelKind, SlotId, plan};
use super::layout::{DataLayout, HANDLER_ENTRIES, LayoutError, PARAMETERS_PER_SLOT, node};
use super::memory::{
    CODE_BASE, CODE_WORDS, DATA_BASE, PlacementError, SharcCore, code_system_address,
};

/// Unity linear gain, the owned default of a new gain node.
const UNITY: u32 = 0x3f80_0000;

/// Owned blocks of a path (b) image: one body per `ResidentKernel::Block` slot.
pub type OwnedBlocks<'a> = BTreeMap<SlotId, &'a dyn OwnedBlock>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResidentConfig<K = KernelKind> {
    pub core: SharcCore,
    /// Protocol core whose ring pair the resident serves (stock SOC1 core 1
    /// is protocol core 0). Must be a ring the ARM drains on this core's
    /// doorbell; see [`ResidentConfig::serves`].
    pub ring: CoreId,
    pub graph: GraphSpec<K>,
    /// Stock module id that BulkStartModel must name to start each slot. A
    /// slot without an id never starts by message (reported failed).
    pub module_ids: BTreeMap<SlotId, u32>,
}

impl ResidentConfig {
    /// The ARM drains ring 1 only on `uio1` (doorbell 98, core 2) and rings 0, 4, 2, 3 on `uio0`
    /// (doorbell 97, core 1).
    pub const fn serves(core: SharcCore, ring: CoreId) -> bool {
        matches!(
            (core, ring),
            (SharcCore::Core2, CoreId::Dsp(DspCore::Core1))
                | (
                    SharcCore::Core1,
                    CoreId::Control | CoreId::Dsp(DspCore::Core0 | DspCore::Core2 | DspCore::Core3)
                )
        )
    }
}

#[derive(Debug, thiserror::Error)]
pub enum FirmwareError {
    #[error(transparent)]
    Graph(#[from] GraphError),
    #[error(transparent)]
    Layout(#[from] LayoutError),
    #[error(transparent)]
    Code(#[from] CodeGenerationError),
    #[error(transparent)]
    Placement(#[from] PlacementError),
    #[error(transparent)]
    Ldr(#[from] LdrError),
    #[error(transparent)]
    Blocks(#[from] BlockKernelError),
    #[error("generated code has {0} words; the owned block-0 range holds {CODE_WORDS}")]
    CodeSize(usize),
    #[error("generated code lacks symbol {0}")]
    Symbol(&'static str),
    #[error("{core:?} cannot serve ring {ring:?}: the ARM drains it on the other doorbell")]
    RingPairing { core: SharcCore, ring: CoreId },
    #[error("module id given for slot {0:?}, which has no node")]
    ModuleSlot(SlotId),
    #[error("slot {0:?} runs an owned block, but none was given")]
    MissingBlock(SlotId),
    #[error("owned block given for slot {0:?}, which has no owned-block node")]
    BlockSlot(SlotId),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum InstallError {
    #[error(transparent)]
    Bus(#[from] BusError),
    #[error(transparent)]
    Program(#[from] ProgramError),
}

/// Everything needed to boot one owned core.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResidentImage<K = KernelKind> {
    core: SharcCore,
    plan: FramePlan<K>,
    layout: DataLayout,
    code: OwnedCode,
    /// Owned block bodies and kernels, contiguous after `code`.
    blocks: BlockCode,
    /// Initialized words from `DATA_BASE` up to the first DMA buffer; the rest
    /// of the layout starts zeroed.
    data: Vec<u32>,
}

impl ResidentImage {
    /// A resident with built-in kernels only.
    pub fn build(config: &ResidentConfig) -> Result<Self, FirmwareError> {
        Self::build_with_blocks(config, &OwnedBlocks::new())
    }
}

impl<K: Kernel> ResidentImage<K> {
    /// A path (b) resident: `blocks` gives the body of every owned-block node.
    /// Node flags start without `node::STARTED`, so each block resets and gets
    /// every delivered parameter word on its first frame, and with
    /// `node::SELF_BYPASS`: the kernel runs the bypass mixer itself.
    pub fn build_with_blocks(
        config: &ResidentConfig<K>,
        blocks: &OwnedBlocks<'_>,
    ) -> Result<Self, FirmwareError> {
        if !ResidentConfig::serves(config.core, config.ring) {
            return Err(FirmwareError::RingPairing {
                core: config.core,
                ring: config.ring,
            });
        }
        let plan = plan(&config.graph)?;
        if let Some(slot) = config
            .module_ids
            .keys()
            .find(|slot| plan.step(**slot).is_none())
        {
            return Err(FirmwareError::ModuleSlot(*slot));
        }
        for step in &plan.steps {
            match (step.kernel.builtin(), blocks.contains_key(&step.slot)) {
                (None, false) => return Err(FirmwareError::MissingBlock(step.slot)),
                (Some(_), true) => return Err(FirmwareError::BlockSlot(step.slot)),
                _ => {}
            }
        }
        if let Some(slot) = blocks.keys().find(|slot| plan.step(**slot).is_none()) {
            return Err(FirmwareError::BlockSlot(*slot));
        }
        let memories: BTreeMap<SlotId, Vec<u32>> = blocks
            .iter()
            .map(|(slot, block)| (*slot, block_memory(*block)))
            .collect();
        let layout = DataLayout::new(
            &plan,
            memories
                .iter()
                .map(|(slot, words)| (*slot, words.len() as u32)),
        )?;
        let operations = bring_up(
            config.core,
            layout.system(config.core, layout.rx_descriptors[0])?,
            layout.system(config.core, layout.tx_descriptors[0])?,
        );
        let code = generate(CodeConfig {
            core: config.core,
            ring: config.ring,
            layout: &layout,
            bring_up: &operations,
        })?;
        let block_code = match layout.stack_top {
            Some(stack_top) => {
                let nodes: Vec<BlockNode> = blocks
                    .iter()
                    .map(|(slot, block)| BlockNode {
                        slot: *slot,
                        block: *block,
                        memory: layout.node_memory[slot],
                    })
                    .collect();
                let origin = PmWordAddress24::new(CODE_BASE + code.word_count() as u32)
                    .map_err(CodeGenerationError::from)?;
                emit_block_kernels(origin, &nodes, stack_top)?
            }
            None => BlockCode::default(),
        };
        let words = code.word_count() + block_code.word_count();
        if words > CODE_WORDS as usize {
            return Err(FirmwareError::CodeSize(words));
        }
        let data = initial_data(config, &plan, &layout, &code, &block_code, &memories)?;
        Ok(Self {
            core: config.core,
            plan,
            layout,
            code,
            blocks: block_code,
            data,
        })
    }

    pub fn core(&self) -> SharcCore {
        self.core
    }
    pub fn plan(&self) -> &FramePlan<K> {
        &self.plan
    }
    pub fn layout(&self) -> &DataLayout {
        &self.layout
    }
    /// The resident program from `CODE_BASE` (vectors, scheduler, handlers).
    pub fn code(&self) -> &OwnedCode {
        &self.code
    }
    pub fn block_code(&self) -> &BlockCode {
        &self.blocks
    }
    pub fn initialized_data(&self) -> &[u32] {
        &self.data
    }
    /// Start address after load: the bring-up routine (reset vector target).
    pub fn entry(&self) -> PmWordAddress24 {
        symbol(&self.code, INIT).expect("generated code defines init")
    }

    /// Copy every code segment into `program` and data into `memory` (word
    /// aliases).
    pub fn install(
        &self,
        program: &mut ProgramMemory,
        memory: &mut impl Bus,
    ) -> Result<(), InstallError> {
        program.load(&self.code)?;
        for segment in &self.blocks.segments {
            program.load(segment)?;
        }
        for (offset, word) in self.data.iter().enumerate() {
            memory.write(DATA_BASE + offset as u32, *word)?;
        }
        for address in DATA_BASE + self.data.len() as u32..self.layout.end {
            memory.write(address, 0)?;
        }
        Ok(())
    }

    /// Code literal, initialized-data literal and a zero fill for buffers.
    /// FIRST/FINAL target the entry, as stock images do.
    pub fn ldr(&self) -> Result<LdrImage, FirmwareError> {
        let mut builder = LdrBuilder::new(self.core.ldr(), self.entry().get());
        let mut code = self.code.isa48_le_bytes();
        for segment in &self.blocks.segments {
            code.extend(segment.isa48_le_bytes());
        }
        // DMACODE 1 blocks are 4-byte multiples; pad half a NOP word.
        code.resize(code.len().next_multiple_of(4), 0);
        builder.literal(
            code_system_address(self.core, PmWordAddress24::new(CODE_BASE).expect("24-bit"))?,
            code,
        )?;
        let data = self
            .data
            .iter()
            .flat_map(|word| word.to_le_bytes())
            .collect();
        builder.literal(self.layout.system(self.core, DATA_BASE)?, data)?;
        let zero_start = DATA_BASE + self.data.len() as u32;
        builder.zero_fill(
            self.layout.system(self.core, zero_start)?,
            4 * (self.layout.end - zero_start),
        )?;
        Ok(builder.finish()?)
    }
}

fn symbol(code: &OwnedCode, name: &'static str) -> Result<PmWordAddress24, FirmwareError> {
    code.symbols()
        .get(&Label::new(name))
        .copied()
        .ok_or(FirmwareError::Symbol(name))
}

fn initial_data<K: Kernel>(
    config: &ResidentConfig<K>,
    plan: &FramePlan<K>,
    layout: &DataLayout,
    code: &OwnedCode,
    blocks: &BlockCode,
    memories: &BTreeMap<SlotId, Vec<u32>>,
) -> Result<Vec<u32>, FirmwareError> {
    let core = config.core;
    let mut words = vec![0; (layout.rx[0] - DATA_BASE) as usize];
    let mut put = |address: u32, value: u32| words[(address - DATA_BASE) as usize] = value;

    for (position, step) in plan.steps.iter().enumerate() {
        let record = layout.nodes + node::WORDS * position as u32;
        let parameters = layout.slot_parameters(step.slot.get());
        let kernel = match step.kernel.builtin() {
            Some(KernelKind::Gain) => {
                put(parameters, UNITY);
                symbol(code, KERNEL_GAIN)?
            }
            Some(KernelKind::Mix) => symbol(code, KERNEL_MIX)?,
            None => {
                for word in 0..PARAMETERS_PER_SLOT {
                    put(parameters + word, NOT_DELIVERED);
                }
                blocks.kernels[&step.slot]
            }
        };
        let module = config.module_ids.get(&step.slot);
        // Never `node::STARTED`: an owned block resets on its first frame.
        let flags = node::ACTIVE
            | if step.kernel.inputs() == 1 {
                node::BYPASSABLE
            } else {
                0
            }
            | if module.is_some() {
                node::MODULE_KNOWN
            } else {
                0
            }
            | if step.kernel.builtin().is_none() {
                node::SELF_BYPASS
            } else {
                0
            };
        put(record + node::MODULE, module.copied().unwrap_or(0));
        put(record + node::KERNEL, kernel.get());
        put(record + node::INPUT0, layout.buffer(step.inputs[0].get()));
        if let Some(second) = step.inputs.get(1) {
            put(record + node::INPUT1, layout.buffer(second.get()));
        }
        put(record + node::OUTPUT, layout.buffer(step.output.get()));
        put(record + node::PARAMETERS, parameters);
        put(record + node::FLAGS, flags);
        put(layout.slot_index + u32::from(step.slot.get()), record);
    }
    for (slot, memory) in memories {
        for (address, word) in (layout.node_memory[slot]..).zip(memory) {
            put(address, *word);
        }
    }
    for (table, entries) in [
        (layout.inputs, &plan.inputs),
        (layout.outputs, &plan.outputs),
    ] {
        put(table, entries.len() as u32);
        for (index, (channel, buffer)) in entries.iter().enumerate() {
            put(table + 1 + 2 * index as u32, u32::from(channel.get()));
            put(table + 2 + 2 * index as u32, layout.buffer(buffer.get()));
        }
    }
    let unsupported = symbol(code, UNSUPPORTED_HANDLER)?.get();
    for entry in 0..HANDLER_ENTRIES {
        put(layout.message_handlers + entry, unsupported);
    }
    for (message, label) in HANDLED {
        put(
            layout.message_handlers + message as u32,
            symbol(code, label)?.get(),
        );
    }
    // Two-descriptor rings: word 0 next descriptor, word 1 buffer (stock order).
    for (descriptors, buffers) in [
        (layout.rx_descriptors, layout.rx),
        (layout.tx_descriptors, layout.tx),
    ] {
        for k in 0..2 {
            put(descriptors[k], layout.system(core, descriptors[1 - k])?);
            put(descriptors[k] + 1, layout.system(core, buffers[k])?);
        }
    }
    Ok(words)
}

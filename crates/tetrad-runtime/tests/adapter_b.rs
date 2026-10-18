//! Adapter B: owned tetrad-blocks bodies as path (b) resident node kernels,
//! called with the frame scheduler's protocol on `Machine` (PRM arithmetic),
//! and path (a) V2 on the same deliveries. The last test runs them inside a
//! complete path (b) image.

use std::iter::once;

use tetrad_blocks::blocks::{gate::SimpleGate, lohicut::LowHighCut, volume::Volume};
use tetrad_blocks::verification::{
    ParameterUpdate, VerificationFrame, standard_frames, verify_module,
};
use tetrad_blocks::{BLOCK_SAMPLES, HostReference, OwnedBlock};
use tetrad_dsp_host::transport::RingLayout;
use tetrad_dsp_protocol::{CoreId, DspCore, MessageType};
use tetrad_loader::{BlockData, Limits, RawLdr};
use tetrad_runtime::firmware::code::{
    BLOCK_STACK_WORDS, BYPASS_RAMP_FRAMES, BlockCode, BlockKernelError, BlockNode, NOT_DELIVERED,
    block_memory, emit_block_kernels,
};
use tetrad_runtime::firmware::graph::{
    Channel, FramePlan, GraphSpec, KernelKind, NodeSpec, ResidentKernel, SlotId, Source, plan,
};
use tetrad_runtime::firmware::image::{FirmwareError, OwnedBlocks, ResidentConfig, ResidentImage};
use tetrad_runtime::firmware::layout::{PARAMETERS_PER_SLOT, control, node};
use tetrad_runtime::firmware::memory::{CODE_BASE, DATA_BASE, MESSAGE_WINDOW, SharcCore};
use tetrad_runtime::firmware::sim::{SimulatedSoc, TdmBlock};
use tetrad_sharc::address::PmWordAddress24;
use tetrad_sharc::emit::IsaProgram;
use tetrad_sharc::isa48::{BranchKind, Condition, Instruction48, MODE1_IRPTEN, MODE1_RND32};
use tetrad_sharc::machine::{
    AddressUnit, Arithmetic, Bus, InterruptBit, Machine, MemoryMap, ProgramMemory, Stop,
};
use tetrad_sharc::math::prm;
use tetrad_sharc::register::{Dag1Index, Dag2Index, Dag2Modify, UniversalReg};

const BUFFERS: u32 = DATA_BASE;
const PARAMETERS: u32 = DATA_BASE + 0x100;
const RECORDS: u32 = DATA_BASE + 0x200;
const NODE_MEMORY: u32 = DATA_BASE + 0x400;
const DATA_WORDS: u32 = 0x800;
/// Separate region: a deeper stack faults on unmapped memory.
const STACK_TOP: u32 = DATA_BASE + 0x1000;
const BLOCKS: u32 = CODE_BASE + 0x40;
/// Scheduler and ring-server registers a kernel must keep (`code.rs` header).
const KEPT_R: [usize; 7] = [9, 10, 11, 12, 13, 14, 15];
const FRAMES: usize = 160;

fn slot(n: u8) -> SlotId {
    SlotId::new(n).unwrap()
}
fn parameters(slot: SlotId) -> u32 {
    PARAMETERS + PARAMETERS_PER_SLOT * u32::from(slot.get())
}
fn buffer(index: u16) -> u32 {
    BUFFERS + BLOCK_SAMPLES as u32 * u32::from(index)
}
/// The node record of `slot` (only `FLAGS` is read by a block kernel).
fn record(slot: SlotId) -> u32 {
    RECORDS + node::WORDS * u32::from(slot.get())
}

/// One frame of `frame.node`: per step `I4` node record, `I0` input, `I1`
/// output, `I3` slot parameters, `CALL (M8, I8)`; then IDLE.
fn driver(plan: &FramePlan<ResidentKernel>, code: &BlockCode) -> IsaProgram {
    let mut program = IsaProgram::new(PmWordAddress24::new(CODE_BASE).unwrap());
    let index = |n| UniversalReg::Index(Dag1Index::new(n).unwrap());
    let (i8, m8) = (Dag2Index::new(8).unwrap(), Dag2Modify::new(8).unwrap());
    let mut instructions = vec![Instruction48::Immediate {
        dst: UniversalReg::Dag2Modify(m8),
        value: 0,
    }];
    for step in &plan.steps {
        for (dst, value) in [
            (index(4), record(step.slot)),
            (index(0), buffer(step.inputs[0].get())),
            (index(1), buffer(step.output.get())),
            (index(3), parameters(step.slot)),
            (UniversalReg::Dag2Index(i8), code.kernels[&step.slot].get()),
        ] {
            instructions.push(Instruction48::Immediate { dst, value });
        }
        instructions.push(Instruction48::IndirectBranch {
            kind: BranchKind::Call,
            condition: Condition::Always,
            index: i8,
            modifier: m8,
        });
    }
    instructions.push(Instruction48::Idle);
    for instruction in instructions {
        program.push(instruction).unwrap();
    }
    program
}

/// `(frame, slot, index, value)` host writes to the slot parameter words.
/// 1.5 and -0.25 must be dropped.
const UPDATES: [(usize, u8, u32, f32); 17] = [
    (0, 3, 0, 0.8),
    (0, 3, 1, 1.0),
    (0, 7, 0, 0.5),
    (0, 7, 1, 0.2),
    (0, 7, 2, 0.25),
    (0, 7, 3, 0.7),
    (0, 7, 4, 0.5),
    (20, 3, 0, 0.3),
    (40, 7, 1, 0.6),
    (50, 3, 1, 0.0),
    (50, 3, 0, 1.5),
    (70, 7, 2, 1.0),
    (70, 7, 4, 0.75),
    (90, 3, 0, 1.0),
    (110, 7, 0, 0.0),
    (110, 7, 4, -0.25),
    (130, 7, 3, 0.1),
];

/// DeleteAllModels + BulkStartModel (the resident clears `node::STARTED`)
/// while both blocks ramp: the next call resets and resends every word.
const RESTART: usize = 95;

/// The chain under test: owned Volume (slot 3) into owned Low-High Cut (slot 7).
fn blocks() -> [(SlotId, &'static dyn HostReference); 2] {
    [(slot(3), &Volume), (slot(7), &LowHighCut)]
}

/// in0 → slot 3 → slot 7 → out0, listed out of order.
fn chain() -> GraphSpec<ResidentKernel> {
    let input = Channel::new(0).unwrap();
    GraphSpec {
        nodes: vec![
            NodeSpec {
                slot: slot(7),
                kernel: ResidentKernel::Block,
                inputs: vec![Source::Node(slot(3))],
            },
            NodeSpec {
                slot: slot(3),
                kernel: ResidentKernel::Block,
                inputs: vec![Source::Input(input)],
            },
        ],
        outputs: vec![(input, Source::Node(slot(7)))],
    }
}

/// What adapter B delivers to `block` in `slot` per frame, as V2 frames
/// (samples still zero): a changed word in index order (out-of-range bits
/// included: both adapters drop them); at [`RESTART`] a reset, then every
/// word again. `lead` frames come first, so every update moves by `lead`.
fn deliveries(at: SlotId, block: &dyn OwnedBlock, lead: usize) -> Vec<VerificationFrame> {
    let mut words = [NOT_DELIVERED; PARAMETERS_PER_SLOT as usize];
    let mut shadows = words;
    (0..FRAMES + lead)
        .map(|frame| {
            for &(_, _, index, value) in UPDATES
                .iter()
                .filter(|update| update.0 + lead == frame && slot(update.1) == at)
            {
                words[index as usize] = value.to_bits();
            }
            let reset = frame == RESTART + lead;
            if reset {
                shadows = [NOT_DELIVERED; PARAMETERS_PER_SLOT as usize];
            }
            let mut parameters = Vec::new();
            for spec in block.parameters() {
                let index = spec.runtime_index as usize;
                if words[index] != shadows[index] {
                    shadows[index] = words[index];
                    parameters.push(ParameterUpdate {
                        index: spec.runtime_index,
                        value: f32::from_bits(words[index]),
                    });
                }
            }
            VerificationFrame {
                samples: [[0.0; BLOCK_SAMPLES]; 2],
                reset,
                parameters,
            }
        })
        .collect()
}

/// One twin frame on channel A: its output and the state after it.
type TwinFrame = ([f32; BLOCK_SAMPLES], Vec<u32>);

/// Runs the twin of `block` over `frames` on channel A, after the first reset.
fn twin(block: &dyn HostReference, frames: &[VerificationFrame]) -> Vec<TwinFrame> {
    let mut state = block.static_layout().states()[0].clone();
    block.reset(&mut state);
    frames
        .iter()
        .map(|frame| {
            if frame.reset {
                block.reset(&mut state);
            }
            for update in &frame.parameters {
                if update.value.to_bits() <= 1.0_f32.to_bits() {
                    block.set_parameter(&mut state, update.index, update.value);
                }
            }
            let mut samples = frame.samples[0];
            block.process(&mut state, &mut samples);
            (samples, state.clone())
        })
        .collect()
}

/// The twins of [`blocks`] in [`chain`] on channel A `input` (`lead` frames,
/// then [`FRAMES`]), after path (a) V2 equals them on the same frames: each
/// block's V2 frames carry its chain input on channel A, the standard
/// channel B, and adapter B's deliveries (review F12).
fn path_a_twins(input: &[[f32; BLOCK_SAMPLES]], lead: usize) -> [Vec<TwinFrame>; 2] {
    let (volume, cut) = (Volume, LowHighCut);
    let standard: Vec<_> = standard_frames()
        .into_iter()
        .cycle()
        .take(input.len())
        .collect();
    let mut schedules = blocks().map(|(slot, block)| deliveries(slot, block, lead));
    for ((frame, a), b) in schedules[0].iter_mut().zip(input).zip(&standard) {
        frame.samples = [*a, b.samples[1]];
    }
    let volume_twin = twin(&volume, &schedules[0]);
    for ((frame, b), (chained, _)) in schedules[1].iter_mut().zip(&standard).zip(&volume_twin) {
        frame.samples = [*chained, b.samples[1]];
    }
    verify_module(&volume, &schedules[0]).unwrap();
    verify_module(&cut, &schedules[1]).unwrap();
    [volume_twin, twin(&cut, &schedules[1])]
}

/// Path (b) equals the twins bit for bit, and path (a) V2 equals the same
/// twins on the same deliveries and the same channel-A input (review F12).
#[test]
fn owned_volume_into_low_high_cut_equals_the_path_a_twins_bit_for_bit() {
    let blocks = blocks();
    let input: Vec<_> = standard_frames()
        .into_iter()
        .cycle()
        .take(FRAMES)
        .map(|frame| frame.samples[0])
        .collect();
    let twins = path_a_twins(&input, 0);
    let plan = plan(&chain()).unwrap();

    let mut memory = MemoryMap::default();
    memory
        .add(DATA_BASE, AddressUnit::Word, DATA_WORDS as usize)
        .unwrap();
    let stack_base = STACK_TOP + 1 - BLOCK_STACK_WORDS;
    memory
        .add(stack_base, AddressUnit::Word, BLOCK_STACK_WORDS as usize)
        .unwrap();
    let mut nodes = Vec::new();
    let mut next = NODE_MEMORY;
    for (slot, block) in blocks {
        let words = block_memory(block);
        for (offset, word) in words.iter().enumerate() {
            memory.write(next + offset as u32, *word).unwrap();
        }
        for word in 0..PARAMETERS_PER_SLOT {
            memory
                .write(parameters(slot) + word, NOT_DELIVERED)
                .unwrap();
        }
        memory
            .write(record(slot) + node::FLAGS, node::ACTIVE)
            .unwrap();
        nodes.push(BlockNode {
            slot,
            block,
            memory: next,
        });
        next += words.len() as u32;
    }
    let code =
        emit_block_kernels(PmWordAddress24::new(BLOCKS).unwrap(), &nodes, STACK_TOP).unwrap();
    let mut program = ProgramMemory::default();
    program
        .load(&driver(&plan, &code).finish().unwrap())
        .unwrap();
    for segment in &code.segments {
        program.load(segment).unwrap();
    }

    let mut machine = Machine::reset();
    machine.arithmetic = Arithmetic::Prm;
    machine.mode1 = MODE1_RND32 | MODE1_IRPTEN;
    let (input_buffer, output_buffer) = (
        buffer(plan.inputs[0].1.get()),
        buffer(plan.outputs[0].1.get()),
    );
    let last_record = record(plan.steps.last().unwrap().slot);
    for (frame, samples) in input.iter().enumerate() {
        if frame == RESTART {
            for (slot, _) in blocks {
                let flags = memory.peek(record(slot) + node::FLAGS).unwrap();
                memory
                    .write(record(slot) + node::FLAGS, flags & !node::STARTED)
                    .unwrap();
            }
        }
        for (offset, sample) in samples.iter().enumerate() {
            memory
                .write(input_buffer + offset as u32, sample.to_bits())
                .unwrap();
        }
        for &(_, n, index, value) in UPDATES.iter().filter(|update| update.0 == frame) {
            // The handler stores any finite value; the adapter guards the range.
            memory
                .write(parameters(slot(n)) + index, value.to_bits())
                .unwrap();
        }
        for (n, register) in KEPT_R.into_iter().enumerate() {
            machine.r[register] = 0x6b00_0000 | n as u32;
        }
        (machine.i[5], machine.m[1]) = (0x6c05, 8);
        let kept = (machine.r, machine.i[5], machine.m[1]);
        machine.set_pc(CODE_BASE);
        let stop = machine.run(&program, &mut memory, None, 100_000).unwrap();
        assert_eq!(stop, Stop::Idle);
        assert_eq!(machine.stkyx, 0, "frame {frame}: AIS/MIS raised");
        for register in KEPT_R {
            assert_eq!(
                machine.r[register], kept.0[register],
                "frame {frame}: R{register}"
            );
        }
        assert_eq!(
            (machine.i[4], machine.i[5], machine.m[1]),
            (last_record, kept.1, kept.2),
            "frame {frame}: I4, I5, M1"
        );

        for (offset, sample) in twins[1][frame].0.iter().enumerate() {
            let actual = memory.peek(output_buffer + offset as u32).unwrap();
            assert_eq!(
                actual,
                sample.to_bits(),
                "frame {frame} sample {offset}: path (b) {:e}, twin {sample:e}",
                f32::from_bits(actual)
            );
        }
        for (node, expected) in nodes.iter().zip(&twins) {
            assert_ne!(
                memory.peek(record(node.slot) + node::FLAGS).unwrap() & node::STARTED,
                0,
                "frame {frame}: slot {:?} not started",
                node.slot
            );
            for (offset, word) in expected[frame].1.iter().enumerate() {
                assert_eq!(
                    memory.peek(node.memory + offset as u32).unwrap(),
                    *word,
                    "frame {frame}: slot {:?} state word {offset}",
                    node.slot
                );
            }
        }
    }
}

#[test]
fn stereo_bodies_and_duplicate_slots_are_refused() {
    let gate = SimpleGate;
    let origin = PmWordAddress24::new(BLOCKS).unwrap();
    let node = |slot, block: &'static dyn OwnedBlock| BlockNode {
        slot,
        block,
        memory: NODE_MEMORY,
    };
    assert!(matches!(
        emit_block_kernels(origin, &[node(slot(1), &SimpleGate)], STACK_TOP),
        Err(BlockKernelError::Stereo(model)) if model == gate.model_id()
    ));
    assert!(matches!(
        emit_block_kernels(origin, &[node(slot(1), &Volume), node(slot(1), &Volume)], STACK_TOP),
        Err(BlockKernelError::DuplicateSlot(duplicate)) if duplicate == slot(1)
    ));
}

const RING: CoreId = CoreId::Dsp(DspCore::Core0);

/// A 24-bit sample left-justified in its 32-bit TDM slot, and the f32 the
/// resident reads from it (`FLOAT` and `* 2^-31` are exact for 24 bits).
fn tdm_sample(value: f32) -> (i32, f32) {
    let q = (value * 8_388_608.0)
        .round()
        .clamp(-8_388_608.0, 8_388_607.0);
    ((q as i32) << 8, q / 8_388_608.0)
}

/// A bulk-shaped message (`+12` record count) on the ARM → SHARC ring, in stock
/// publication order: payload, length, then the producer byte.
fn publish<const WORDS: usize>(
    soc: &mut SimulatedSoc,
    message: MessageType,
    records: &[[u32; WORDS]],
) {
    let len = 16 + 4 * WORDS as u32 * records.len() as u32;
    let mut bytes = vec![message as u8, 0, 0, 0, 8, 0, 0, 0];
    bytes.extend(len.to_le_bytes());
    bytes.extend((records.len() as u32).to_le_bytes());
    bytes.extend(records.iter().flatten().flat_map(|word| word.to_le_bytes()));
    let layout = RingLayout::outgoing(RING);
    let at = |offset: usize| MESSAGE_WINDOW + offset as u32;
    let memory = soc.memory_mut();
    let producer = memory.read_bytes(at(layout.producer_offset), 1).unwrap()[0];
    let slot = (usize::from(producer) + 1) % usize::from(layout.slot_count);
    memory
        .load_bytes(at(layout.payload_offset + slot * layout.slot_bytes), &bytes)
        .unwrap();
    memory
        .load_bytes(at(layout.length_offset + 4 * slot), &len.to_le_bytes())
        .unwrap();
    memory
        .load_bytes(at(layout.producer_offset), &[slot as u8])
        .unwrap();
}

/// `(frame, value)`: BulkModelBypass of Low-High Cut (slot 7); the kernel
/// sees it one frame later, like [`UPDATES`]. Bypass, active, then bypass with
/// mute while the active ramp runs (a reversal from partial gains); the
/// [`RESTART`] comes during the mute ramp; then active again.
const BYPASSES: [(usize, f32); 4] = [(10, 1.0), (60, 0.0), (70, 2.0), (120, 0.0)];
const BYPASS_SLOT: u8 = 7;

/// Host twin of the adapter-B bypass mixer (PRM f32 computes). Mode 0 active,
/// 1 bypass, 2 bypass with mute.
struct Mixer {
    mode: u32,
    gains: [f32; 2],
    steps: [f32; 2],
    left: u32,
}

impl Mixer {
    /// Stock targets `[dry, wet]` (k1 `0x1659b0`).
    fn targets(mode: u32) -> [f32; 2] {
        [
            f32::from(u8::from(mode == 1)),
            f32::from(u8::from(mode == 0)),
        ]
    }
    fn settled(mode: u32) -> Self {
        Self {
            mode,
            gains: Self::targets(mode),
            steps: [0.0; 2],
            left: 0,
        }
    }
    fn set(&mut self, mode: u32) {
        if mode != self.mode {
            let targets = Self::targets(mode);
            self.steps = [0, 1].map(|n| prm::mul(prm::sub(targets[n], self.gains[n]), 1.0 / 960.0));
            (self.mode, self.left) = (mode, BYPASS_RAMP_FRAMES);
        }
    }
    fn frame(
        &mut self,
        dry: &[f32; BLOCK_SAMPLES],
        wet: &[f32; BLOCK_SAMPLES],
    ) -> [f32; BLOCK_SAMPLES] {
        if self.left == 0 {
            return [*wet, *dry, [0.0; BLOCK_SAMPLES]][self.mode as usize];
        }
        self.left -= 1;
        let out = std::array::from_fn(|n| {
            let [g_dry, g_wet] = self.gains;
            let sample = prm::add(prm::mul(dry[n], g_dry), prm::mul(wet[n], g_wet));
            self.gains = [0, 1].map(|k| prm::add(self.gains[k], self.steps[k]));
            sample
        });
        if self.left == 0 {
            self.gains = Self::targets(self.mode);
        }
        out
    }
}

/// Path (b) end to end: the image for [`chain`] boots on `Machine` (PRM
/// arithmetic) with `SimulatedSoc`, takes 24-bit TDM input through
/// SPORT/DMA/SEC, and gets [`UPDATES`], [`BYPASSES`] and the [`RESTART`]
/// (DeleteAllModels + BulkStartModel) through the stock ring. Both block states
/// equal the twins every frame, so a bypassed block keeps processing (stock).
/// The output equals the [`Mixer`] twin over the twin outputs: the Low-High
/// Cut twin while active and settled, the Volume twin while bypassed and
/// settled. Path (a) V2 equals the twins on the same frames.
#[test]
fn owned_image_runs_volume_into_low_high_cut_and_its_bypass_like_the_twins() {
    // The resident polls the ring after the graph: a message sent before
    // frame f reaches the blocks in frame f + 1. Image frame 0 is the first
    // reset on silence; everything else is one frame later than above.
    let lead = 1;
    let (tdm, input): (Vec<_>, Vec<_>) = once([0.0; BLOCK_SAMPLES])
        .chain(
            standard_frames()
                .into_iter()
                .cycle()
                .take(FRAMES)
                .map(|frame| frame.samples[0]),
        )
        .map(|samples| {
            let converted = samples.map(tdm_sample);
            (converted.map(|(word, _)| word), converted.map(|(_, f)| f))
        })
        .unzip();
    let twins = path_a_twins(&input, lead);

    let blocks = blocks();
    let owned: OwnedBlocks = blocks
        .iter()
        .map(|&(slot, block)| (slot, block as &dyn OwnedBlock))
        .collect();
    let config = ResidentConfig {
        core: SharcCore::Core1,
        ring: RING,
        graph: chain(),
        module_ids: blocks
            .iter()
            .map(|(slot, block)| (*slot, block.model_id()))
            .collect(),
    };
    let image = ResidentImage::build_with_blocks(&config, &owned).unwrap();
    let layout = image.layout();
    // The LDR carries every segment, contiguous from the vector aperture, and
    // the code runs past the 0x2800 bytes the stock LDR writes in block 0.
    let ldr = image.ldr().unwrap();
    let parsed = RawLdr::parse(ldr.bytes(), Limits::default()).unwrap();
    let mut code = Vec::new();
    for block in parsed.blocks() {
        let target = block.header.target_address();
        if let BlockData::Literal(bytes) = block.data
            && (0x2824_0000..0x282c_0000).contains(&target)
        {
            assert_eq!(target, 0x2824_0000 + code.len() as u32);
            code.extend_from_slice(bytes);
        }
    }
    let mut expected = image.code().isa48_le_bytes();
    for segment in &image.block_code().segments {
        expected.extend(segment.isa48_le_bytes());
    }
    assert!(expected.len() > 0x2800);
    assert_eq!(&code[..expected.len()], expected);
    let mut soc = SimulatedSoc::new(SharcCore::Core1);
    let mut program = ProgramMemory::default();
    image.install(&mut program, &mut soc).unwrap();
    let record = |soc: &SimulatedSoc, slot: SlotId| {
        soc.memory()
            .peek(layout.slot_index + u32::from(slot.get()))
            .unwrap()
    };
    for (slot, _) in blocks {
        let flags = soc.memory().peek(record(&soc, slot) + node::FLAGS).unwrap();
        assert_eq!(
            flags & (node::ACTIVE | node::STARTED | node::MODULE_KNOWN | node::SELF_BYPASS),
            node::ACTIVE | node::MODULE_KNOWN | node::SELF_BYPASS
        );
        for word in 0..PARAMETERS_PER_SLOT {
            let parameter = layout.slot_parameters(slot.get()) + word;
            assert_eq!(soc.memory().peek(parameter).unwrap(), NOT_DELIVERED);
        }
    }

    let mut machine = Machine::reset();
    machine.arithmetic = Arithmetic::Prm;
    machine.set_pc(image.entry().get());
    soc.start();
    assert_eq!(
        machine.run(&program, &mut soc, None, 100_000).unwrap(),
        Stop::Idle
    );
    let output = layout.buffer(image.plan().outputs[0].1.get());
    let mut messages = 0;
    let mut mixer = Mixer::settled(0);
    for (frame, samples) in tdm.iter().enumerate() {
        if let Some(&(_, value)) = BYPASSES.iter().find(|bypass| bypass.0 + lead == frame + 1) {
            publish(
                &mut soc,
                MessageType::BulkModelBypass,
                &[[u32::from(BYPASS_SLOT), value.to_bits()]],
            );
            messages += 1;
        }
        if let Some(&(_, value)) = BYPASSES.iter().find(|bypass| bypass.0 + lead == frame) {
            mixer.set(value as u32);
        }
        if frame == RESTART + lead {
            mixer = Mixer::settled(mixer.mode);
        }
        let expected = mixer.frame(&twins[0][frame].0, &twins[1][frame].0);
        let updates: Vec<[u32; 3]> = UPDATES
            .iter()
            .filter(|update| update.0 + lead == frame + 1)
            .map(|&(_, n, index, value)| [u32::from(n), index, value.to_bits()])
            .collect();
        if !updates.is_empty() {
            publish(&mut soc, MessageType::BulkParameterChange, &updates);
            messages += 1;
        }
        if RESTART + lead == frame + 1 {
            publish::<0>(&mut soc, MessageType::DeleteAllModels, &[]);
            let starts: Vec<[u32; 3]> = blocks
                .iter()
                .map(|(slot, block)| [block.model_id(), u32::from(slot.get()), u32::MAX])
                .collect();
            publish(&mut soc, MessageType::BulkStartModel, &starts);
            messages += 2;
        }
        let mut block: TdmBlock = [[0; 8]; BLOCK_SAMPLES];
        for (row, word) in block.iter_mut().zip(samples) {
            row[0] = *word;
        }
        soc.transfer_frame(&block).unwrap();
        machine.raise(InterruptBit::SECI);
        assert_eq!(
            machine.run(&program, &mut soc, None, 100_000).unwrap(),
            Stop::Idle,
            "frame {frame}"
        );
        assert_eq!(machine.stkyx, 0, "frame {frame}: AIS/MIS raised");

        for (offset, sample) in expected.iter().enumerate() {
            let actual = soc.memory().peek(output + offset as u32).unwrap();
            assert_eq!(
                actual,
                sample.to_bits(),
                "frame {frame} sample {offset}: image {:e}, twin {sample:e}",
                f32::from_bits(actual)
            );
        }
        // The restart messages clear the flag after this frame's graph.
        let started = if RESTART + lead == frame + 1 {
            0
        } else {
            node::STARTED
        };
        for ((slot, _), expected) in blocks.iter().zip(&twins) {
            let flags = soc
                .memory()
                .peek(record(&soc, *slot) + node::FLAGS)
                .unwrap();
            assert_eq!(flags & node::STARTED, started, "frame {frame}: {slot:?}");
            let memory = layout.node_memory[slot];
            for (address, word) in (memory..).zip(&expected[frame].1) {
                assert_eq!(
                    soc.memory().peek(address).unwrap(),
                    *word,
                    "frame {frame}: {slot:?} state word {}",
                    address - memory
                );
            }
        }
    }
    let control = |word| soc.memory().peek(layout.control + word).unwrap();
    assert_eq!(control(control::MESSAGES), messages);
    assert_eq!(
        [control::REJECTED, control::UNSUPPORTED, control::FAULT].map(control),
        [0; 3]
    );
}

/// An image needs exactly one body per owned-block node, and none elsewhere.
#[test]
fn image_needs_one_body_per_owned_block_node() {
    let config = ResidentConfig {
        core: SharcCore::Core1,
        ring: RING,
        graph: chain(),
        module_ids: Default::default(),
    };
    let build = |bodies: &[(u8, &'static dyn OwnedBlock)]| {
        let owned: OwnedBlocks = bodies.iter().map(|&(n, body)| (slot(n), body)).collect();
        ResidentImage::build_with_blocks(&config, &owned)
    };
    assert!(matches!(
        build(&[(3, &Volume)]),
        Err(FirmwareError::MissingBlock(missing)) if missing == slot(7)
    ));
    assert!(matches!(
        build(&[(3, &Volume), (7, &LowHighCut), (9, &Volume)]),
        Err(FirmwareError::BlockSlot(extra)) if extra == slot(9)
    ));
    let mut gain = config.clone();
    gain.graph.nodes[1].kernel = ResidentKernel::Builtin(KernelKind::Gain);
    let owned: OwnedBlocks = [
        (slot(3), &Volume as &dyn OwnedBlock),
        (slot(7), &LowHighCut),
    ]
    .into_iter()
    .collect();
    assert!(matches!(
        ResidentImage::build_with_blocks(&gain, &owned),
        Err(FirmwareError::BlockSlot(builtin)) if builtin == slot(3)
    ));
}

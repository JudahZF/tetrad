//! qcdsp's generated blocks (the `qcdsp` crate in the qcdsp submodule) run on this crate's raw
//! SHARC+ machine, which shares no code with qcdsp's encoder (`qc-asm`) or its text decoder.
//! The tests moved here from qcdsp, so that qcdsp does not depend on Tetrad.
use std::ops::Range;

use qc_format::{blob::Section, diff::DiffFile};
use qcdsp::{
    Block, Channels, Processor,
    device::{
        Generated, Heap, KernelMap, Layout, VolumeTemplate,
        template::{STOCK_DESTROY, STOCK_INIT, STOCK_PARCELS},
    },
    examples,
};
use tetrad_sharc::{
    isa48::{MODE1_CBUFEN, MODE1_IRPTEN, MODE1_NESTM, MODE1_RND32},
    machine::{
        AddressUnit, Arithmetic, Bus, BusError, Machine, MachineError, MemoryMap, ProgramMemory,
        Stop,
    },
};

const CODE: u32 = 0x1c_0000;
const IO: u32 = 0x2c_0600;
const STACK: u32 = 0x24_a7f0;
const RETURN: u32 = 0x16_6072;
const MODE: u32 = MODE1_CBUFEN | MODE1_IRPTEN | MODE1_NESTM | MODE1_RND32;
const CANARY: u32 = 0x7fc0_dead;
/// Core 1 kernel `heap_malloc(R4 heap, R8 bytes) -> R0` and `heap_free(R4 heap, R8 pointer)`
/// (dsp-heap note §2.1). The harness answers calls to them; nothing else may be called.
const HEAP_MALLOC: u32 = 0x16_adfa;
const HEAP_FREE: u32 = 0x16_ad7e;
/// The PM heap of core 1 (heap 1, L1 block 2), byte space: a network's weights.
const WEIGHTS: u32 = 0x30_4000;

struct Memory {
    map: MemoryMap,
    writes: Vec<u32>,
}

impl Bus for Memory {
    fn write_byte(&mut self, address: u32, value: u8) -> Result<(), BusError> {
        self.writes.push(address);
        self.map.write_byte(address, value)
    }
    fn read(&mut self, address: u32) -> Result<u32, BusError> {
        self.map.read(address)
    }
    fn write(&mut self, address: u32, value: u32) -> Result<(), BusError> {
        self.writes.push(address);
        self.map.write(address, value)
    }
}

/// One call into the kernel heap: (function, R4, R8).
type HeapCall = (u32, u32, u32);

struct Harness {
    cpu: Machine,
    program: ProgramMemory,
    memory: Memory,
    state: u32,
    scale: u32,
    layout: Layout,
    generated: Generated,
    /// The arena region and its address scale, mapped when the block has an arena.
    arena: Option<(u32, u32)>,
    /// What heap_malloc returns: the arena, or 0 (heap full).
    allocation: u32,
    /// A network's weights region (heap 1, PM; byte space) and what heap_malloc(1) returns.
    weights: Option<u32>,
    weight_allocation: u32,
    heap_calls: Vec<HeapCall>,
}

impl Harness {
    fn new(block: &Block, byte_state: bool) -> Self {
        let (layout, generated) = qcdsp::device::generate(block).unwrap();
        // The loader relocates each kind-7 return literal (Type 16a DATA32) to the load address.
        let mut parcels = generated.parcels.clone();
        for relocation in &generated.relocations {
            assert_eq!(
                (relocation.kind, relocation.target_base),
                (7, Section::Code.base())
            );
            let at = (relocation.offset - STOCK_PARCELS) as usize;
            assert_eq!(parcels[at], 0x9fc0, "DM(I7,M7) = imm32");
            let value = CODE + relocation.target_offset;
            (parcels[at + 1], parcels[at + 2]) = ((value >> 16) as u16, value as u16);
        }
        let mut program = ProgramMemory::default();
        program.load_visa(CODE + STOCK_PARCELS, &parcels).unwrap();
        let (state, scale, unit) = if byte_state {
            (0x2e_1800, 4, AddressUnit::Byte)
        } else {
            (0xb_0000, 1, AddressUnit::Word)
        };
        let mut map = MemoryMap::default();
        map.add(state, unit, layout.total_words as usize).unwrap();
        map.add(IO, AddressUnit::Byte, 2).unwrap();
        // Deliberately separate channels and mix their address units.
        map.add(0x24_1080, AddressUnit::Byte, 32).unwrap();
        map.add(0xc_0000, AddressUnit::Word, 32).unwrap();
        map.add(STACK - 2044, AddressUnit::Byte, 512).unwrap();
        map.write(IO, 0x24_1080).unwrap();
        map.write(IO + 4, 0xc_0000).unwrap();
        // The arena takes the other address unit from the state, so neither depends on it.
        let arena = layout.arena.map(|arena| {
            let (base, scale, unit) = match arena.heap {
                // The SD heap of core 1 (DDR, byte space).
                Heap::Sd => (0x8000_0000, 4, AddressUnit::Byte),
                Heap::Dm if byte_state => (0xd_0000, 1, AddressUnit::Word),
                Heap::Dm => (0x25_0000, 4, AddressUnit::Byte),
                Heap::Pm => unreachable!("only weights are in heap 1"),
            };
            map.add(base, unit, arena.words as usize).unwrap();
            for word in 0..arena.words {
                map.write(base + scale * word, CANARY).unwrap();
            }
            (base, scale)
        });
        let weights = layout.weights().map(|weights| {
            map.add(WEIGHTS, AddressUnit::Byte, weights.words as usize)
                .unwrap();
            for word in 0..weights.words {
                map.write(WEIGHTS + 4 * word, CANARY).unwrap();
            }
            WEIGHTS
        });
        Self {
            cpu: Machine::reset(),
            program,
            memory: Memory {
                map,
                writes: Vec::new(),
            },
            state,
            scale,
            layout,
            generated,
            allocation: arena.map_or(0, |(base, _)| base),
            arena,
            weight_allocation: weights.unwrap_or(0),
            weights,
            heap_calls: Vec::new(),
        }
    }

    /// Run a callback as the kernel calls it (stock C call, I6/I7 frame). `stock` is the stock
    /// routine it must tail-jump to with R4 = state and the entry frame, or None for a C return.
    fn call(&mut self, entry: u32, index: u32, value: u32, stock: Option<u32>) {
        let cpu = &mut self.cpu;
        cpu.arithmetic = Arithmetic::Prm;
        cpu.r = std::array::from_fn(|n| 0x5a5a_0000 | n as u32);
        cpu.extension = [0; 16];
        cpu.i = std::array::from_fn(|n| 0x2_0000 + n as u32 * 0x100);
        cpu.m = std::array::from_fn(|n| 0x40 + n as i32);
        cpu.l = [0; 16];
        for (zero, one, minus_one) in [(5, 6, 7), (13, 14, 15)] {
            (cpu.m[zero], cpu.m[one], cpu.m[minus_one]) = (0, 1, -1);
        }
        cpu.mode1 = MODE;
        cpu.stkyx = 0;
        [cpu.r[4], cpu.r[8], cpu.r[12]] = [self.state, index, value];
        (cpu.i[6], cpu.i[7]) = (STACK + 4, STACK);
        let before = (cpu.r, cpu.i, cpu.m);
        self.memory.map.write(STACK, cpu.i[6]).unwrap();
        self.memory.map.write(STACK - 4, RETURN - 1).unwrap();
        (cpu.i[6], cpu.i[7]) = (STACK, STACK - 8);
        self.memory.writes.clear();
        let target = stock.map_or(RETURN, |stock| CODE + stock);
        self.cpu.set_pc(CODE + entry);
        let start = self.cpu.cycles();
        loop {
            if [HEAP_MALLOC, HEAP_FREE].contains(&self.cpu.pc()) {
                self.kernel();
                continue;
            }
            let cpu = &mut self.cpu;
            match cpu.run(&self.program, &mut self.memory, Some(target), 1) {
                Ok(Stop::Breakpoint(_)) => break,
                Err(MachineError::StepBudget(_)) if cpu.cycles() - start < 2_000_000 => {}
                result => panic!("callback {entry:#x}, PC {:#x}: {result:?}", cpu.pc()),
            }
            // The interrupt stack is shared: nothing live may be at/below SP.
            for word in 0..8 {
                self.memory.map.write(cpu.i[7] - 4 * word, CANARY).unwrap();
            }
        }
        let cpu = &self.cpu;
        for n in [3, 5, 6, 7, 9, 10, 11, 13, 14, 15] {
            assert_eq!(cpu.r[n], before.0[n], "R{n}");
        }
        for n in (0..16).filter(|n| ![4, 12, 13].contains(n)) {
            let expected = match (stock, n) {
                // Stock init and destroy start with the frame of the kernel's call.
                (Some(_), 6) => STACK,
                (Some(_), 7) => STACK - 8,
                _ => before.1[n],
            };
            assert_eq!(cpu.i[n], expected, "I{n}");
        }
        if stock.is_some() {
            assert_eq!(cpu.r[4], self.state, "stock code gets R4 = state");
        }
        for n in (0..16).filter(|n| ![4, 12].contains(n)) {
            assert_eq!(cpu.m[n], before.2[n], "M{n}");
        }
        assert_eq!(cpu.mode1, MODE);
        assert_eq!(cpu.l, [0; 16], "C code runs with every DAG length zero");
        assert_eq!(cpu.stkyx & 0x220, 0, "AIS/MIS faults the kernel slot");
        assert_eq!(
            cpu.stkyy & 0x220,
            0,
            "AIS/MIS on PEy faults the kernel slot"
        );
        if entry == self.generated.entries.process {
            assert!(
                cpu.cycles() - start <= self.generated.cycles_per_call,
                "modeled process instructions exceed the resource estimate"
            );
        }
        assert_eq!(
            cpu.pc_stack_depth() + cpu.loop_stack_depth() + cpu.status_stack_depth(),
            0
        );
        let arena = self.arena.map_or(0..0, |(base, scale)| {
            base..base + scale * self.layout.arena.unwrap().words
        });
        let weights = self.weights.map_or(0..0, |base| {
            base..base + 4 * self.layout.weights().unwrap().words
        });
        let writable: [Range<u32>; 6] = [
            weights,
            self.state + 17 * self.scale..self.state + self.layout.total_words * self.scale,
            arena,
            0x24_1080..0x24_1100,
            0xc_0000..0xc_0020,
            STACK - 2044..STACK - 4,
        ];
        assert!(
            self.memory
                .writes
                .iter()
                .all(|address| writable.iter().any(|r| r.contains(address))),
            "stray writes: {:?}",
            self.memory.writes
        );
    }

    /// The kernel side of a stock C call to heap_malloc or heap_free: check the frame the
    /// caller built, record the arguments, clobber the stack below SP and every scratch
    /// register, then do the C return (`I12 = DM(M7, I6)`, jump to I12 + 1, RFRAME).
    fn kernel(&mut self) {
        let cpu = &mut self.cpu;
        let function = cpu.pc();
        assert_eq!(cpu.i[7], cpu.i[6] - 8, "CJUMP slots push R2 and the return");
        let caller_frame = self.memory.map.peek(cpu.i[6]).unwrap();
        let back = self.memory.map.peek(cpu.i[6] - 4).unwrap() + 1;
        assert!(
            caller_frame > cpu.i[6],
            "the caller's frame is above the callee's"
        );
        self.heap_calls.push((function, cpu.r[4], cpu.r[8]));
        for word in 0..64 {
            self.memory.map.write(cpu.i[7] - 4 * word, CANARY).unwrap();
        }
        cpu.r[0] = if function == HEAP_MALLOC && cpu.r[4] == 1 {
            self.weight_allocation
        } else if function == HEAP_MALLOC {
            self.allocation
        } else {
            CANARY
        };
        for n in [1, 2, 4, 8, 12] {
            cpu.r[n] = CANARY ^ n as u32;
        }
        (cpu.i[4], cpu.i[12], cpu.i[13]) = (CANARY, CANARY, CANARY);
        (cpu.m[4], cpu.m[12]) = (CANARY as i32, CANARY as i32);
        cpu.i[7] = cpu.i[6];
        cpu.i[6] = caller_frame;
        cpu.set_pc(back);
    }

    fn arena_word(&self, word: u32) -> u32 {
        let (base, scale) = self.arena.unwrap();
        self.memory.map.peek(base + scale * word).unwrap()
    }

    fn state_word(&self, word: u32) -> u32 {
        self.memory
            .map
            .peek(self.state + self.scale * word)
            .unwrap()
    }

    /// Fill both channels with frame `frame` of the test signal; returns the input.
    fn write_input(&mut self, frame: usize) -> [[f32; 32]; 2] {
        std::array::from_fn(|channel| {
            std::array::from_fn(|sample| {
                let input = ((frame * 32 + sample + 7 * channel) as f32 * 0.03125).sin()
                    * if channel == 0 { 0.3 } else { -0.7 };
                let (base, scale) = CHANNEL_BUFFERS[channel];
                self.memory
                    .map
                    .write(base + scale * sample as u32, input.to_bits())
                    .unwrap();
                input
            })
        })
    }

    fn output(&self, channel: usize, sample: usize) -> f32 {
        let (base, scale) = CHANNEL_BUFFERS[channel];
        f32::from_bits(self.memory.map.peek(base + scale * sample as u32).unwrap())
    }
}

/// Channel A (byte alias) and channel B (normal-word alias) buffers of `io`.
const CHANNEL_BUFFERS: [(u32, u32); 2] = [(0x24_1080, 4), (0xc_0000, 1)];

#[test]
fn raw_callbacks_use_both_io_pointers_and_survive_interrupt_stack_writes() {
    let blocks = [
        examples::gain().unwrap(),
        examples::soft_clip().unwrap(),
        examples::delay(7).unwrap(),
        examples::biquad([0.06745527, 0.13491054, 0.06745527, -1.1429805, 0.4128016]).unwrap(),
        examples::tremolo().unwrap(),
        examples::envelope_filter().unwrap(),
        examples::sweep_synth(examples::Voicing::Guitar).unwrap(),
        examples::sweep_synth(examples::Voicing::Bass).unwrap(),
        examples::saturator().unwrap(),
    ];
    for block in blocks {
        let mono = block.channels() == Channels::Mono;
        for byte_state in [false, true] {
            let mut h = Harness::new(&block, byte_state);
            h.call(h.generated.entries.init, 0, 0, Some(STOCK_INIT));
            let mut twins = [
                Processor::new(block.clone()).unwrap(),
                Processor::new(block.clone()).unwrap(),
            ];
            for (index, p) in block.parameters().iter().enumerate() {
                h.call(
                    h.generated.entries.set_parameter,
                    index as u32,
                    0.375_f32.to_bits(),
                    None,
                );
                for twin in &mut twins {
                    twin.set_parameter(index, p.min + 0.375 * (p.max - p.min))
                        .unwrap();
                }
            }
            for frame in 0..40 {
                if frame == 20 {
                    // A fault can leave every history and cursor unusable.
                    // Recovery must repair both banks without resetting controls.
                    for word in h.layout.banks[0]..h.layout.total_words {
                        h.memory
                            .map
                            .write(h.state + h.scale * word, CANARY)
                            .unwrap();
                    }
                    h.call(h.generated.entries.recovery, 0xdead_beef, 0x7fc0_dead, None);
                    for twin in &mut twins {
                        twin.reset();
                    }
                }
                let input = h.write_input(frame);
                // A mono block computes channel A and writes it to both channels.
                let expected: [[f32; 32]; 2] = std::array::from_fn(|channel| {
                    let source = if mono { 0 } else { channel };
                    if mono && channel == 1 {
                        return [f32::NAN; 32];
                    }
                    input[source].map(|x| twins[source].process_sample(x))
                });
                let expected = if mono {
                    [expected[0], expected[0]]
                } else {
                    expected
                };
                h.call(h.generated.entries.process, IO, 0, None);
                check_output(&h, &expected, block.name(), frame);
            }
        }
    }
}

fn check_output(h: &Harness, expected: &[[f32; 32]; 2], name: &str, frame: usize) {
    for (channel, samples) in expected.iter().enumerate() {
        for (sample, expected) in samples.iter().enumerate() {
            let actual = h.output(channel, sample);
            assert!(
                (actual - expected).abs() <= 2e-5 * expected.abs().max(1.0),
                "{name}: frame {frame}, channel {channel}, sample {sample}: {actual} vs {expected}",
            );
        }
    }
}

/// A stereo delay of 9000 samples needs 72 KB of banks, more than Volume's 0x7fff-byte state:
/// they go into a heap-0 arena.
fn arena_block() -> Block {
    examples::delay(9_000).unwrap()
}

/// The arena blocks on the raw machine, with the kernel heap answered by the harness: init
/// allocates once with the arena's heap and size, zeroes the arena and sets the cursors; process
/// matches the host reference across the whole delay line; fault recovery repairs the poisoned
/// arena; destroy frees the same pointer from the same heap, then runs the stock destroy. A
/// 65536-sample stereo delay (512 KB) does not fit heap 0, so its arena is in the SD heap.
#[test]
fn arena_block_allocates_runs_recovers_and_frees_its_heap_memory() {
    for (samples, heap) in [(9_000, Heap::Dm), (65_536, Heap::Sd)] {
        arena_lifecycle(&examples::delay(samples).unwrap(), samples as u32, heap);
    }
}

fn arena_lifecycle(block: &Block, samples: u32, heap: Heap) {
    let frames = samples / 32 + 20;
    for byte_state in [false, true] {
        let mut h = Harness::new(block, byte_state);
        let arena = h.layout.arena.expect("banks larger than the state");
        assert_eq!(arena.heap, heap);
        let (base, _) = h.arena.unwrap();
        assert!(arena.bytes() > 0x7fff);
        h.call(h.generated.entries.init, 0, 0, Some(STOCK_INIT));
        assert_eq!(h.heap_calls, [(HEAP_MALLOC, heap as u32, arena.bytes())]);
        assert_eq!(h.state_word(arena.pointer), base);
        let cursors: Vec<u32> = h
            .layout
            .banks
            .iter()
            .map(|bank| bank + h.layout.cursors)
            .collect();
        for word in 0..arena.words {
            let expected = if cursors.contains(&word) { samples } else { 0 };
            assert_eq!(h.arena_word(word), expected, "arena word {word}");
        }

        let mut twins = [
            Processor::new(block.clone()).unwrap(),
            Processor::new(block.clone()).unwrap(),
        ];
        for (index, p) in block.parameters().iter().enumerate() {
            h.call(
                h.generated.entries.set_parameter,
                index as u32,
                0.375_f32.to_bits(),
                None,
            );
            for twin in &mut twins {
                twin.set_parameter(index, p.min + 0.375 * (p.max - p.min))
                    .unwrap();
            }
        }
        // The frames before the recovery pass the delay line and its wrap.
        for frame in 0..frames + 20 {
            if frame == frames {
                let (base, scale) = h.arena.unwrap();
                for word in 0..arena.words {
                    h.memory.map.write(base + scale * word, CANARY).unwrap();
                }
                h.call(h.generated.entries.recovery, 0, 0, None);
                for twin in &mut twins {
                    twin.reset();
                }
            }
            let input = h.write_input(frame as usize);
            let expected: [[f32; 32]; 2] = std::array::from_fn(|channel| {
                input[channel].map(|x| twins[channel].process_sample(x))
            });
            h.call(h.generated.entries.process, IO, 0, None);
            check_output(&h, &expected, block.name(), frame as usize);
        }
        assert_eq!(h.heap_calls.len(), 1, "only init allocates");

        let destroy = h.generated.entries.destroy.expect("arena blocks free it");
        h.call(destroy, 0, 0, Some(STOCK_DESTROY));
        assert_eq!(h.heap_calls[1..], [(HEAP_FREE, heap as u32, base)]);
        assert!(
            h.memory
                .writes
                .iter()
                .all(|a| (STACK - 2044..STACK).contains(a))
        );
    }
}

/// A two-array WaveNet network (IR 3): init takes the DM arena (bank, frame buffers, rings) and
/// the PM weights arena and writes the packed weights; process runs the frame kernels in SIMD
/// and matches the host reference past the longest dilation; recovery clears the history;
/// destroy frees both heaps.
#[test]
fn network_block_runs_its_frame_kernels() {
    let block = examples::wavenet(&[
        examples::WavenetArray {
            input: 1,
            channels: 4,
            head: 2,
            head_bias: false,
            dilations: vec![1, 2, 4, 8],
        },
        examples::WavenetArray {
            input: 4,
            channels: 2,
            head: 1,
            head_bias: true,
            dilations: vec![1, 2, 16],
        },
    ])
    .unwrap();
    for byte_state in [false, true] {
        let mut h = Harness::new(&block, byte_state);
        let arena = h.layout.arena.expect("a network has an arena");
        let weights = h.layout.weights().expect("and weights");
        let plan = h.layout.network.clone().unwrap().plan;
        h.call(h.generated.entries.init, 0, 0, Some(STOCK_INIT));
        assert_eq!(
            h.heap_calls,
            [
                (HEAP_MALLOC, 0, arena.bytes()),
                (HEAP_MALLOC, 1, weights.bytes())
            ]
        );
        for (word, value) in plan.weights.iter().enumerate() {
            let stored = h.memory.map.peek(WEIGHTS + 4 * word as u32).unwrap();
            assert_eq!(stored, value.to_bits(), "weight {word}");
        }

        let mut twin = Processor::new(block.clone()).unwrap();
        for (index, value) in [(0, 0.6_f32), (1, 0.3)] {
            h.call(
                h.generated.entries.set_parameter,
                index,
                value.to_bits(),
                None,
            );
            let p = &block.parameters()[index as usize];
            twin.set_parameter(index as usize, p.min + value * (p.max - p.min))
                .unwrap();
        }
        // 40 calls pass the longest history (2 × 16 samples) many times over.
        let mut peak = 0f32;
        for frame in 0..48 {
            if frame == 40 {
                h.call(h.generated.entries.recovery, 0, 0, None);
                twin.reset();
            }
            let input = h.write_input(frame);
            let expected = input[0].map(|x| twin.process_sample(x));
            peak = expected.iter().fold(peak, |peak, v| peak.max(v.abs()));
            h.call(h.generated.entries.process, IO, 0, None);
            check_output(&h, &[expected, expected], block.name(), frame);
        }
        assert!(peak > 0.01, "the network makes a signal: peak {peak}");
        let destroy = h
            .generated
            .entries
            .destroy
            .expect("a network frees its heaps");
        h.call(destroy, 0, 0, Some(STOCK_DESTROY));
        let base = h.arena.unwrap().0;
        assert_eq!(
            h.heap_calls[2..],
            [(HEAP_FREE, 1, WEIGHTS), (HEAP_FREE, 0, base)]
        );
    }
}

/// The less common kernel paths: a convolution that accumulates into its output, a fused
/// condition on a one-term convolution (its loads need instructions of their own), odd channel
/// counts, relu and hardtanh, and a gain.
#[test]
fn network_kernels_cover_accumulate_and_short_convolutions() {
    use qcdsp::{Activation, BlockBuilder, Category, NetworkBuilder, Parameter};
    let mut dsp = BlockBuilder::new("net", "Net", Category::Utility);
    dsp.parameter(Parameter::new("Level", 0.0, 2.0, 1.0).unwrap())
        .unwrap();
    let mut net = NetworkBuilder::new();
    let x = net.input();
    net.gain(x, 0);
    let a = net.buffer(3);
    // One term (1 input, kernel 1) with a fused condition (x, accumulate).
    net.conv(
        x,
        a,
        1,
        1,
        vec![0.5, -0.25, 0.75],
        Some(vec![0.1, 0.0, -0.1]),
        false,
    );
    net.conv(x, a, 1, 1, vec![0.2, 0.3, -0.4], None, true);
    net.activation(a, Activation::Relu);
    let b = net.buffer(3);
    net.conv(
        a,
        b,
        2,
        3,
        (0..18).map(|n| n as f32 * 0.05 - 0.4).collect(),
        None,
        false,
    );
    // Accumulates into b with history: the residual is the output itself.
    net.conv(
        a,
        b,
        3,
        1,
        (0..27).map(|n| 0.3 - n as f32 * 0.02).collect(),
        None,
        true,
    );
    net.activation(b, Activation::Hardtanh);
    let y = net.buffer(1);
    net.conv(b, y, 1, 1, vec![0.6, -0.5, 0.4], Some(vec![0.05]), false);
    let output = dsp.network(net.finish(y)).unwrap();
    let block = dsp.finish(output).unwrap();
    for byte_state in [false, true] {
        let mut h = Harness::new(&block, byte_state);
        h.call(h.generated.entries.init, 0, 0, Some(STOCK_INIT));
        h.call(
            h.generated.entries.set_parameter,
            0,
            0.8_f32.to_bits(),
            None,
        );
        let mut twin = Processor::new(block.clone()).unwrap();
        twin.set_parameter(0, 1.6).unwrap();
        let mut peak = 0f32;
        for frame in 0..8 {
            let input = h.write_input(frame);
            let expected = input[0].map(|x| twin.process_sample(x));
            peak = expected.iter().fold(peak, |peak, v| peak.max(v.abs()));
            h.call(h.generated.entries.process, IO, 0, None);
            check_output(&h, &[expected, expected], block.name(), frame);
        }
        assert!(peak > 0.01, "the network makes a signal: peak {peak}");
    }
}

/// A network whose DM arena allocation fails plays dry, and destroy frees nothing, even when
/// the state's weights pointer word held a stale value before init.
#[test]
fn failed_network_allocation_frees_nothing() {
    let block = examples::wavenet(&[examples::WavenetArray {
        input: 1,
        channels: 2,
        head: 1,
        head_bias: true,
        dilations: vec![1, 2],
    }])
    .unwrap();
    let mut h = Harness::new(&block, true);
    h.allocation = 0;
    let weights = h.layout.weights().unwrap();
    let stale = h.state + h.scale * weights.pointer;
    h.memory.map.write(stale, 0xdead_beef).unwrap();
    h.call(h.generated.entries.init, 0, 0, Some(STOCK_INIT));
    assert_eq!(h.state_word(weights.pointer), 0);
    let input = h.write_input(0);
    h.call(h.generated.entries.process, IO, 0, None);
    for (channel, samples) in input.iter().enumerate() {
        for (sample, x) in samples.iter().enumerate() {
            assert_eq!(h.output(channel, sample).to_bits(), x.to_bits(), "dry");
        }
    }
    h.call(
        h.generated.entries.destroy.unwrap(),
        0,
        0,
        Some(STOCK_DESTROY),
    );
    assert!(
        h.heap_calls
            .iter()
            .all(|&(function, _, _)| function == HEAP_MALLOC),
        "{:x?}",
        h.heap_calls
    );
}

/// heap_malloc returns 0 when heap 0 is full. Then nothing touches the arena: process leaves
/// the buffers as they are (dry) and writes only its own frame, recovery writes nothing, and
/// destroy goes straight to the stock destroy.
#[test]
fn failed_arena_allocation_plays_dry_and_writes_nothing() {
    let block = arena_block();
    let mut h = Harness::new(&block, true);
    h.allocation = 0;
    let arena = h.layout.arena.unwrap();
    h.call(h.generated.entries.init, 0, 0, Some(STOCK_INIT));
    assert_eq!(h.heap_calls, [(HEAP_MALLOC, 0, arena.bytes())]);
    assert_eq!(h.state_word(arena.pointer), 0);
    let state = h.state..h.state + h.layout.bytes();
    let frame = STACK - 2044..STACK;
    assert!(
        h.memory
            .writes
            .iter()
            .all(|a| state.contains(a) || frame.contains(a))
    );
    for index in 0..3 {
        let input = h.write_input(index);
        h.call(h.generated.entries.process, IO, 0, None);
        for (channel, samples) in input.iter().enumerate() {
            for (sample, x) in samples.iter().enumerate() {
                assert_eq!(h.output(channel, sample).to_bits(), x.to_bits());
            }
        }
        assert!(h.memory.writes.iter().all(|a| frame.contains(a)));
    }
    h.call(h.generated.entries.recovery, 0, 0, None);
    assert!(h.memory.writes.is_empty());
    h.call(
        h.generated.entries.destroy.unwrap(),
        0,
        0,
        Some(STOCK_DESTROY),
    );
    assert!(h.memory.writes.is_empty());
    assert_eq!(h.heap_calls.len(), 1, "no heap_free of a null arena");
    for word in 0..arena.words {
        assert_eq!(h.arena_word(word), CANARY);
    }
}

#[test]
fn invalid_parameters_do_not_write_state_or_raise_float_faults() {
    let mut h = Harness::new(&examples::gain().unwrap(), true);
    h.call(h.generated.entries.init, 0, 0, Some(STOCK_INIT));
    let address = h.state + h.layout.parameters * h.scale;
    let original = h.memory.map.peek(address).unwrap();
    for bits in [
        0x7fc0_0000,
        0x7f80_0001,
        0xffc0_0000,
        0x7f80_0000,
        0xff80_0000,
        0x8000_0000,
        (-0.1_f32).to_bits(),
        1.01_f32.to_bits(),
    ] {
        h.call(h.generated.entries.set_parameter, 0, bits, None);
        assert_eq!(h.memory.map.peek(address).unwrap(), original);
        assert!(h.memory.writes.is_empty());
    }
    for index in [1, 99, u32::MAX] {
        h.call(
            h.generated.entries.set_parameter,
            index,
            0.5_f32.to_bits(),
            None,
        );
        assert!(h.memory.writes.is_empty());
    }
    // Gain maps normalized +0..=1 onto 0..=4. Positive denormals are
    // accepted by the integer guard and flushed to zero by PRM arithmetic.
    for (bits, expected) in [(0, 0.0_f32), (1, 0.0), (1.0_f32.to_bits(), 4.0)] {
        h.call(h.generated.entries.set_parameter, 0, bits, None);
        assert_eq!(h.memory.map.peek(address).unwrap(), expected.to_bits());
        assert_eq!(h.memory.writes, [address]);
    }
}

/// The stock Volume template from `QC_CORPUS_DIR` (docs/development.md, "Corpus tests"): the
/// decrypted core 1 blob in `plain/blobs` and the core 2-4 diff files of the root file system.
/// `None` with a note when it is not set.
fn template() -> Option<VolumeTemplate> {
    let Some(dir) = std::env::var_os("QC_CORPUS_DIR") else {
        eprintln!("skipped: set QC_CORPUS_DIR");
        return None;
    };
    let dir = std::path::Path::new(&dir);
    let core1 = std::fs::read(dir.join("plain/blobs/core1/volume.raw")).unwrap();
    let diffs = [2, 3, 4].map(|core| {
        let path = dir.join(format!("rootfs/opt/neuraldsp/blobs/core{core}/diffs.blob"));
        DiffFile::parse(&std::fs::read(path).unwrap())
            .unwrap()
            .get("volume")
            .unwrap()
            .clone()
    });
    Some(VolumeTemplate::new(&core1, diffs).unwrap())
}

/// With the stock Volume: the generated entry, relocated to each core's origin, installs the
/// recovery callback pointer into ctx word 14 and nothing else, on all four cores.
#[test]
fn entry_installs_relocated_recovery_on_all_four_cores() {
    let Some(template) = template() else {
        return;
    };
    let block = examples::delay(7).unwrap();
    let built = qcdsp::device::build(&block, &template, 4_000_000_000, block.name()).unwrap();
    let images = std::iter::once(built.core1.clone()).chain(
        built
            .diffs
            .iter()
            .map(|diff| diff.apply(&built.core1).unwrap()),
    );
    for image in images {
        for origin in [0x12_0000, 0x16_0000, 0x1c_0000] {
            let mut parcels: Vec<u16> = image
                .content(Section::Code)
                .unwrap()
                .chunks_exact(2)
                .map(|b| u16::from_le_bytes([b[0], b[1]]))
                .collect();
            let entry = image.entry - Section::Code.base();
            let relocation = image
                .relocations
                .iter()
                .find(|group| group.section == Section::Code)
                .unwrap()
                .code_relocations()
                .unwrap()
                .into_iter()
                .find(|r| r.offset == entry + 2)
                .unwrap();
            assert_eq!(
                (
                    relocation.kind,
                    relocation.target_base,
                    relocation.target_offset
                ),
                (8, Section::Code.base(), built.entries.recovery)
            );
            let pointer = origin + relocation.target_offset;
            parcels[entry as usize + 3] = (pointer >> 16) as u16;
            parcels[entry as usize + 4] = pointer as u16;
            let mut program = ProgramMemory::default();
            program.load_visa(origin, &parcels).unwrap();
            let mut memory = MemoryMap::default();
            let context = 0x2c_0690;
            memory.add(context, AddressUnit::Byte, 21).unwrap();
            for word in 0..21 {
                memory.write(context + 4 * word, 0xdead_beef).unwrap();
            }
            let mut machine = Machine::reset();
            machine.r[4] = context;
            machine.set_pc(origin + entry);
            assert_eq!(
                machine.run(&program, &mut memory, Some(origin + 0x150), 10),
                Ok(Stop::Breakpoint(origin + 0x150))
            );
            assert_eq!(machine.r[4], context);
            for word in 0..21 {
                assert_eq!(
                    memory.peek(context + 4 * word).unwrap(),
                    if word == 14 { pointer } else { 0xdead_beef }
                );
            }
        }
    }
}

/// With the stock Volume and a kernel map harvested from the stock digital_delay records (as a
/// build does): the arena block links, its records apply and validate on cores 2-4, each kernel
/// call holds that core's heap_malloc or heap_free address (checked against the research map,
/// `kernel_call_map.json`), each return literal has its kind-7 relocation, and ctx[8] points at
/// the generated destroy.
#[test]
#[ignore = "set QC_CORPUS_DIR"]
fn arena_block_links_with_each_cores_heap_calls() {
    let Some(template) = template() else {
        return;
    };
    let dir = std::path::PathBuf::from(std::env::var_os("QC_CORPUS_DIR").unwrap());
    let block = arena_block();
    assert!(
        qcdsp::device::build(&block, &template, 4_000_000_000, block.name()).is_err(),
        "an arena block needs the kernel map"
    );
    let source = qcdsp::device::template::KERNEL_CALL_SOURCE;
    let core1 = qc_format::blob::ModuleBlob::parse(
        &std::fs::read(dir.join(format!("plain/blobs/core1/{source}.raw"))).unwrap(),
    )
    .unwrap();
    let diffs = [2, 3, 4].map(|core| {
        let path = dir.join(format!("rootfs/opt/neuraldsp/blobs/core{core}/diffs.blob"));
        DiffFile::parse(&std::fs::read(path).unwrap())
            .unwrap()
            .get(source)
            .unwrap()
            .clone()
    });
    let mut kernel = KernelMap::default();
    kernel.add_stock_calls(&core1, &diffs).unwrap();
    let research = KernelMap::from_json(
        &std::fs::read_to_string(dir.join("plain/kernel_call_map.json")).unwrap(),
    )
    .unwrap();
    let template = template.with_kernel(kernel);
    let built = qcdsp::device::build(&block, &template, 4_000_000_000, block.name()).unwrap();
    let (_, generated) = qcdsp::device::generate(&block).unwrap();
    assert_eq!(generated.kernel_refs.len(), 2);

    let arena = built.layout.arena.unwrap();
    let charge = |bytes: u32| (bytes + 11) & !7;
    let dm_heap = 208 - charge(0x44) + charge(built.layout.bytes()) + charge(arena.bytes());
    assert!(
        built
            .module_element
            .contains(&format!("dm_heap=\"{dm_heap}\""))
    );

    let images = std::iter::once(built.core1.clone()).chain(built.diffs.iter().map(|diff| {
        let image = diff.apply(&built.core1).unwrap();
        image.validate().unwrap();
        image
    }));
    for (core, image) in images.enumerate() {
        let code: Vec<u16> = image
            .content(Section::Code)
            .unwrap()
            .chunks_exact(2)
            .map(|b| u16::from_le_bytes([b[0], b[1]]))
            .collect();
        let relocations = image
            .relocations
            .iter()
            .find(|group| group.section == Section::Code)
            .unwrap()
            .code_relocations()
            .unwrap();
        assert!(
            relocations.is_sorted_by_key(|r| r.offset),
            "stock record order"
        );
        let has = |kind, offset, target| {
            relocations.iter().any(|r| {
                (r.kind, r.offset, r.target_base, r.target_offset)
                    == (kind, offset, Section::Code.base(), target)
            })
        };
        for call in &generated.kernel_refs {
            let at = call.parcel as usize;
            let expected = match core {
                0 => call.core1_address,
                n => research.get(call.core1_address).unwrap()[n - 1],
            };
            assert_eq!(
                code[at..at + 3],
                [0x1804, (expected >> 16) as u16, expected as u16],
                "core {} CJUMP at {at:#x}",
                core + 1
            );
            // DM(I7,M7) = R2, then the return literal: next instruction - 1, relocated.
            assert_eq!(code[at + 3], 0x9ff2);
            let return_minus_one = call.parcel + 6;
            assert_eq!(
                code[at + 4..at + 7],
                [
                    0x9fc0,
                    0x00f8,
                    (Section::Code.base() + return_minus_one) as u16
                ]
            );
            assert!(has(7, call.parcel + 4, return_minus_one));
        }
        let destroy = built.entries.destroy.unwrap();
        assert!(has(8, 0x174, destroy));
        assert_eq!(code[0x175..0x177], [0x00f8, destroy as u16]);
    }
    let addresses: Vec<u32> = generated
        .kernel_refs
        .iter()
        .map(|call| call.core1_address)
        .collect();
    assert_eq!(addresses, [HEAP_MALLOC, HEAP_FREE]);
}

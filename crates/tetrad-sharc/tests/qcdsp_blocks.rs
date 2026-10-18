//! qcdsp's generated blocks (the `qcdsp` crate in the qcdsp submodule) run on this crate's raw
//! SHARC+ machine, which shares no code with qcdsp's encoder (`qc-asm`) or its text decoder.
//! The tests moved here from qcdsp, so that qcdsp does not depend on Tetrad.
use std::ops::Range;

use qc_format::{blob::Section, diff::DiffFile};
use qcdsp::{
    Block, Processor,
    device::{
        Generated, Layout, VolumeTemplate,
        template::{STOCK_INIT, STOCK_PARCELS},
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

struct Harness {
    cpu: Machine,
    program: ProgramMemory,
    memory: Memory,
    state: u32,
    scale: u32,
    layout: Layout,
    generated: Generated,
}

impl Harness {
    fn new(block: &Block, byte_state: bool) -> Self {
        let (layout, generated) = qcdsp::device::generate(block).unwrap();
        let mut program = ProgramMemory::default();
        program
            .load_visa(CODE + STOCK_PARCELS, &generated.parcels)
            .unwrap();
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
        }
    }

    fn call(&mut self, entry: u32, index: u32, value: u32, construction: bool) {
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
        let target = if construction {
            CODE + STOCK_INIT
        } else {
            RETURN
        };
        cpu.set_pc(CODE + entry);
        let start = cpu.cycles();
        loop {
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
        for n in [3, 5, 6, 7, 9, 10, 11, 13, 14, 15] {
            assert_eq!(cpu.r[n], before.0[n], "R{n}");
        }
        for n in (0..16).filter(|n| ![4, 12, 13].contains(n)) {
            if construction && [6, 7].contains(&n) {
                continue;
            }
            assert_eq!(cpu.i[n], before.1[n], "I{n}");
        }
        for n in (0..16).filter(|n| ![4, 12].contains(n)) {
            assert_eq!(cpu.m[n], before.2[n], "M{n}");
        }
        assert_eq!(cpu.mode1, MODE);
        assert_eq!(cpu.stkyx & 0x220, 0, "AIS/MIS faults the kernel slot");
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
        let writable: [Range<u32>; 4] = [
            self.state + 17 * self.scale..self.state + self.layout.total_words * self.scale,
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
}

#[test]
fn raw_callbacks_use_both_io_pointers_and_survive_interrupt_stack_writes() {
    let blocks = [
        examples::gain().unwrap(),
        examples::soft_clip().unwrap(),
        examples::delay(7).unwrap(),
        examples::biquad([0.06745527, 0.13491054, 0.06745527, -1.1429805, 0.4128016]).unwrap(),
        examples::tremolo().unwrap(),
        examples::envelope_filter().unwrap(),
        examples::micro_synth(examples::Voicing::Guitar).unwrap(),
        examples::micro_synth(examples::Voicing::Bass).unwrap(),
    ];
    for block in blocks {
        for byte_state in [false, true] {
            let mut h = Harness::new(&block, byte_state);
            h.call(h.generated.entries.init, 0, 0, true);
            let mut twins = [
                Processor::new(block.clone()).unwrap(),
                Processor::new(block.clone()).unwrap(),
            ];
            for (index, p) in block.parameters().iter().enumerate() {
                h.call(
                    h.generated.entries.set_parameter,
                    index as u32,
                    0.375_f32.to_bits(),
                    false,
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
                    h.call(
                        h.generated.entries.recovery,
                        0xdead_beef,
                        0x7fc0_dead,
                        false,
                    );
                    for twin in &mut twins {
                        twin.reset();
                    }
                }
                let expected: [[f32; 32]; 2] = std::array::from_fn(|channel| {
                    std::array::from_fn(|sample| {
                        let input = ((frame * 32 + sample + 7 * channel) as f32 * 0.03125).sin()
                            * if channel == 0 { 0.3 } else { -0.7 };
                        let (base, scale) = if channel == 0 {
                            (0x24_1080, 4)
                        } else {
                            (0xc_0000, 1)
                        };
                        h.memory
                            .map
                            .write(base + scale * sample as u32, input.to_bits())
                            .unwrap();
                        twins[channel].process_sample(input)
                    })
                });
                h.call(h.generated.entries.process, IO, 0, false);
                for (channel, samples) in expected.iter().enumerate() {
                    let (base, scale) = if channel == 0 {
                        (0x24_1080, 4)
                    } else {
                        (0xc_0000, 1)
                    };
                    for (sample, expected) in samples.iter().enumerate() {
                        let actual = f32::from_bits(
                            h.memory.map.peek(base + scale * sample as u32).unwrap(),
                        );
                        assert!(
                            (actual - expected).abs() <= 2e-5 * expected.abs().max(1.0),
                            "{}: frame {frame}, channel {channel}, sample {sample}: {actual} vs {expected}",
                            block.name()
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn invalid_parameters_do_not_write_state_or_raise_float_faults() {
    let mut h = Harness::new(&examples::gain().unwrap(), true);
    h.call(h.generated.entries.init, 0, 0, true);
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
        h.call(h.generated.entries.set_parameter, 0, bits, false);
        assert_eq!(h.memory.map.peek(address).unwrap(), original);
        assert!(h.memory.writes.is_empty());
    }
    for index in [1, 99, u32::MAX] {
        h.call(
            h.generated.entries.set_parameter,
            index,
            0.5_f32.to_bits(),
            false,
        );
        assert!(h.memory.writes.is_empty());
    }
    // Gain maps normalized +0..=1 onto 0..=4. Positive denormals are
    // accepted by the integer guard and flushed to zero by PRM arithmetic.
    for (bits, expected) in [(0, 0.0_f32), (1, 0.0), (1.0_f32.to_bits(), 4.0)] {
        h.call(h.generated.entries.set_parameter, 0, bits, false);
        assert_eq!(h.memory.map.peek(address).unwrap(), expected.to_bits());
        assert_eq!(h.memory.writes, [address]);
    }
}

/// The stock Volume template from `QC_CORPUS_DIR` (README, "Corpus tests"): the decrypted core 1
/// blob in `plain/blobs` and the core 2-4 diff files of the root file system. `None` with a note
/// when it is not set.
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

//! Resident machine model: new ISA forms, loops, interrupts and the bus.
use std::num::NonZeroU16;

use tetrad_sharc::address::DmWordAddress32;
use tetrad_sharc::address::PmWordAddress24;
use tetrad_sharc::emit::{BranchRelocation, EmitError, IsaProgram, Label, LoopCount, OwnedCode};
use tetrad_sharc::isa48::{
    BitOperation, BranchKind, Condition, DmAccess, EncodeError, IMASK_SECI, Instruction48,
    InterruptMask, MODE1_IRPTEN, Mmask, Mode1Mask, SystemBitMask,
};
use tetrad_sharc::machine::{
    AddressUnit, BusError, InterruptBit, Machine, MachineError, MemoryMap, ProgramMemory,
    RegionError, SYSCTL, SYSCTL_IIVT, Stop,
};
use tetrad_sharc::register::{
    Dag1Index, Dag1Modify, Dag2Index, Dag2Modify, DataReg, FloatReg, SystemRegister, UniversalReg,
};

fn r(n: u8) -> DataReg {
    DataReg::new(n).unwrap()
}
fn f(n: u8) -> FloatReg {
    FloatReg::new(n).unwrap()
}
fn imm(n: u8, value: u32) -> Instruction48 {
    Instruction48::Immediate {
        dst: UniversalReg::Data(r(n)),
        value,
    }
}
fn pm(address: u32) -> PmWordAddress24 {
    PmWordAddress24::new(address).unwrap()
}
fn count(n: u16) -> LoopCount {
    LoopCount::Immediate(NonZeroU16::new(n).unwrap())
}

/// PRM formulas; `P+O` rows also match the assembler oracle, `P` rows are
/// PRM-only (oracle layout or UREG table disagrees). See the slice record.
#[test]
fn new_forms_encode_to_prm_vectors() {
    let i8 = Dag2Index::new(8).unwrap();
    let vectors = [
        // P+O
        (
            Instruction48::IntegerAdd {
                dst: r(3),
                lhs: r(5),
                rhs: r(6),
            },
            0x013e00001356,
        ),
        (
            Instruction48::IntegerIncrement {
                dst: r(3),
                src: r(5),
            },
            0x013e00029350,
        ),
        (
            Instruction48::And {
                dst: r(3),
                lhs: r(5),
                rhs: r(6),
            },
            0x013e00040356,
        ),
        (
            Instruction48::Xor {
                dst: r(3),
                lhs: r(5),
                rhs: r(6),
            },
            0x013e00042356,
        ),
        (
            Instruction48::IntegerCompare {
                lhs: r(5),
                rhs: r(6),
            },
            0x013e0000a056,
        ),
        (
            Instruction48::IntegerCompareUnsigned {
                lhs: r(5),
                rhs: r(6),
            },
            0x013e0000b056,
        ),
        (
            Instruction48::FloatFromInteger {
                dst: f(3),
                src: r(5),
            },
            0x013e000ca350,
        ),
        (
            Instruction48::FixToInteger {
                dst: r(3),
                src: f(5),
            },
            0x013e000c9350,
        ),
        (
            Instruction48::LogicalShift {
                dst: r(3),
                src: r(5),
                amount: 4,
            },
            0x023e00000435,
        ),
        (Instruction48::PushStatus, 0x172000000000),
        (Instruction48::Idle, 0x008000000000),
        (
            Instruction48::IndirectBranch {
                kind: BranchKind::Jump,
                condition: Condition::Always,
                index: i8,
                modifier: Dag2Modify::new(8).unwrap(),
            },
            0x083e00000000,
        ),
        // P (+ stock DATAEX sign fill); the oracle leaves DATAEX zero.
        (
            Instruction48::LogicalShift {
                dst: r(3),
                src: r(5),
                amount: -4,
            },
            0x023e7800fc35,
        ),
        // P: the oracle puts the index at 26:24.
        (
            Instruction48::IndirectBranch {
                kind: BranchKind::Call,
                condition: Condition::Always,
                index: Dag2Index::new(12).unwrap(),
                modifier: Dag2Modify::new(9).unwrap(),
            },
            0x08bf08000000,
        ),
        // P+D: Type14d byte store (VISA-titled form, ISA legality open).
        (
            Instruction48::DmDirectByteStore {
                src: r(0),
                address: 0x2008_0010,
            },
            0x1b0020080010,
        ),
        // P: system UREG table (the oracle uses legacy codes).
        (
            Instruction48::Immediate {
                dst: UniversalReg::System(SystemRegister::InterruptLatch),
                value: 0,
            },
            0x0f7a00000000,
        ),
        (
            Instruction48::Immediate {
                dst: UniversalReg::System(SystemRegister::LoopCounter),
                value: 5,
            },
            0x0f6800000005,
        ),
        (
            Instruction48::Immediate {
                dst: UniversalReg::Dag2Index(i8),
                value: 0x1000,
            },
            0x0f1800001000,
        ),
    ];
    for (instruction, word) in vectors {
        assert_eq!(
            instruction.encode().unwrap().bits(),
            word,
            "{instruction:?}"
        );
    }
    let clock = UniversalReg::System(SystemRegister::EmulatorClock);
    assert_eq!(
        Instruction48::Immediate {
            dst: clock,
            value: 0
        }
        .encode(),
        Err(EncodeError::ReadOnlyRegister(clock))
    );
}

#[test]
fn counted_loops_link_to_type12a_and_reject_backward_ends() {
    let mut program = IsaProgram::new(pm(0x1000));
    program
        .counted_loop(count(0x1234), Label::new("end"))
        .unwrap();
    program.push(Instruction48::Nop).unwrap();
    program.push(Instruction48::Nop).unwrap();
    program.define(Label::new("end")).unwrap();
    program.push(Instruction48::Nop).unwrap();
    program
        .counted_loop(LoopCount::Register(r(5)), Label::new("end2"))
        .unwrap();
    program.push(Instruction48::Nop).unwrap();
    program.define(Label::new("end2")).unwrap();
    program.push(Instruction48::Nop).unwrap();
    let code = program.finish().unwrap();
    assert_eq!(code.words()[0].bits(), 0x0c1234000003);
    assert_eq!(code.words()[4].bits(), 0x0d0500000002);

    let mut backward = IsaProgram::new(pm(0x1000));
    backward.define(Label::new("top")).unwrap();
    backward.push(Instruction48::Nop).unwrap();
    backward.counted_loop(count(2), Label::new("top")).unwrap();
    assert!(matches!(backward.finish(), Err(EmitError::LoopEnd { .. })));
}

fn build(origin: u32, emit: impl FnOnce(&mut IsaProgram)) -> OwnedCode {
    let mut program = IsaProgram::new(pm(origin));
    emit(&mut program);
    program.finish().unwrap()
}

fn data_memory() -> MemoryMap {
    let mut memory = MemoryMap::default();
    memory.add(0xb_0000, AddressUnit::Word, 64).unwrap();
    memory.add(0x2008_8000, AddressUnit::Byte, 16).unwrap();
    memory
}

/// Nested counter loops with a CALL in the body; LCNTR from a register.
#[test]
fn nested_loops_and_calls_run_the_exact_iteration_count() {
    let code = build(0x9_0004, |p| {
        p.push(imm(0, 0)).unwrap();
        p.push(imm(1, 1)).unwrap();
        p.push(imm(2, 3)).unwrap();
        p.counted_loop(LoopCount::Register(r(2)), Label::new("outer"))
            .unwrap();
        p.counted_loop(count(4), Label::new("inner")).unwrap();
        p.push(Instruction48::Nop).unwrap();
        p.branch_to(
            BranchKind::Call,
            Condition::Always,
            Label::new("add"),
            BranchRelocation::PcRel24,
        )
        .unwrap();
        p.push(Instruction48::Nop).unwrap();
        p.push(Instruction48::Nop).unwrap();
        p.define(Label::new("inner")).unwrap();
        p.push(Instruction48::Nop).unwrap();
        p.push(Instruction48::Nop).unwrap();
        p.define(Label::new("outer")).unwrap();
        p.push(Instruction48::Nop).unwrap();
        p.push(Instruction48::Idle).unwrap();
        p.define(Label::new("add")).unwrap();
        p.push(Instruction48::IntegerAdd {
            dst: r(0),
            lhs: r(0),
            rhs: r(1),
        })
        .unwrap();
        p.push(Instruction48::Rts).unwrap();
    });
    let mut program = ProgramMemory::default();
    program.load(&code).unwrap();
    let mut machine = Machine::reset();
    let mut memory = data_memory();
    assert_eq!(
        machine.run(&program, &mut memory, None, 10_000),
        Ok(Stop::Idle)
    );
    assert_eq!(machine.r[0], 12);
    assert_eq!(
        (machine.loop_stack_depth(), machine.pc_stack_depth()),
        (0, 0)
    );
}

#[test]
fn loop_restrictions_and_zero_counts_are_explicit_errors() {
    let tail_branch = build(0x9_0004, |p| {
        p.counted_loop(count(2), Label::new("end")).unwrap();
        p.push(Instruction48::Nop).unwrap();
        p.branch_to(
            BranchKind::Jump,
            Condition::NotEqual,
            Label::new("end"),
            BranchRelocation::PcRel24,
        )
        .unwrap();
        p.push(Instruction48::Nop).unwrap();
        p.define(Label::new("end")).unwrap();
        p.push(Instruction48::Nop).unwrap();
    });
    let zero = build(0x9_0004, |p| {
        p.counted_loop(LoopCount::Register(r(3)), Label::new("end"))
            .unwrap();
        p.push(Instruction48::Nop).unwrap();
        p.define(Label::new("end")).unwrap();
        p.push(Instruction48::Nop).unwrap();
    });
    for (code, expected) in [
        (tail_branch, MachineError::BranchInLoopTail(0x9_0006)),
        (zero, MachineError::ZeroLoopCount(0x9_0004)),
    ] {
        let mut program = ProgramMemory::default();
        program.load(&code).unwrap();
        let result = Machine::reset().run(&program, &mut data_memory(), None, 100);
        assert_eq!(result, Err(expected));
    }
}

/// IDLE until SECI: vector entry pushes PC and status, applies MMASK, sets
/// IMASKP; RTI restores MODE1 and resumes after IDLE. A second latch waits
/// while NESTM is clear. SYSCTL.IIVT, set by a normal DM store, selects the
/// internal vector table.
#[test]
fn interrupt_entry_masks_status_and_rti_restores_it() {
    let main = build(0x9_0004, |p| {
        let sysctl = DmWordAddress32::new(SYSCTL);
        p.push(Instruction48::DmDirectLoad {
            dst: r(2),
            address: sysctl,
        })
        .unwrap();
        p.push(imm(3, SYSCTL_IIVT)).unwrap();
        p.push(Instruction48::Or {
            dst: r(2),
            lhs: r(2),
            rhs: r(3),
        })
        .unwrap();
        p.push(Instruction48::DmDirectStore {
            src: r(2),
            address: sysctl,
        })
        .unwrap();
        p.push(Instruction48::MmaskImmediate(
            Mmask::new(MODE1_IRPTEN).unwrap(),
        ))
        .unwrap();
        p.push(Instruction48::SystemBit {
            operation: BitOperation::Set,
            operand: SystemBitMask::Interrupt(InterruptMask::new(IMASK_SECI).unwrap()),
        })
        .unwrap();
        p.push(Instruction48::Mode1Set(
            Mode1Mask::new(MODE1_IRPTEN).unwrap(),
        ))
        .unwrap();
        p.push(Instruction48::Idle).unwrap();
        p.push(imm(1, 7)).unwrap();
        p.push(Instruction48::Idle).unwrap();
    });
    let handler = build(InterruptBit::SECI.vector(), |p| {
        p.push(Instruction48::IntegerIncrement {
            dst: r(0),
            src: r(0),
        })
        .unwrap();
        p.push(Instruction48::Rti).unwrap();
    });
    let mut program = ProgramMemory::default();
    program.load(&main).unwrap();
    program.load(&handler).unwrap();
    let mut machine = Machine::reset();
    let mut memory = data_memory();
    assert_eq!(
        machine.run(&program, &mut memory, None, 100),
        Ok(Stop::Idle)
    );
    assert_eq!((machine.pc(), machine.sysctl), (0x9_000c, SYSCTL_IIVT));

    machine.raise(InterruptBit::SECI);
    let at_handler = machine.run(
        &program,
        &mut memory,
        Some(InterruptBit::SECI.vector() + 1),
        10,
    );
    assert_eq!(
        at_handler,
        Ok(Stop::Breakpoint(InterruptBit::SECI.vector() + 1))
    );
    assert_eq!(machine.mode1 & MODE1_IRPTEN, 0, "MMASK cleared IRPTEN");
    assert_eq!((machine.imaskp, machine.irptl), (IMASK_SECI, 0));
    assert_eq!(machine.status_stack_depth(), 1);
    machine.raise(InterruptBit::SECI);

    // RTI restores IRPTEN, the second latch is serviced, then IDLE+1 runs.
    assert_eq!(
        machine.run(&program, &mut memory, None, 100),
        Ok(Stop::Idle)
    );
    assert_eq!((machine.r[0], machine.r[1], machine.pc()), (2, 7, 0x9_000e));
    assert_eq!(machine.mode1 & MODE1_IRPTEN, MODE1_IRPTEN);
    assert_eq!((machine.imaskp, machine.status_stack_depth()), (0, 0));
}

#[test]
fn indirect_call_and_byte_store_use_dag2_and_byte_lanes() {
    let code = build(0x9_0004, |p| {
        p.push(Instruction48::Immediate {
            dst: UniversalReg::Dag2Index(Dag2Index::new(8).unwrap()),
            value: 0x9_0010,
        })
        .unwrap();
        p.push(Instruction48::Immediate {
            dst: UniversalReg::Dag2Modify(Dag2Modify::new(8).unwrap()),
            value: 2,
        })
        .unwrap();
        p.push(Instruction48::IndirectBranch {
            kind: BranchKind::Call,
            condition: Condition::Always,
            index: Dag2Index::new(8).unwrap(),
            modifier: Dag2Modify::new(8).unwrap(),
        })
        .unwrap();
        p.push(imm(4, 0x1234_56ab)).unwrap();
        p.push(Instruction48::DmDirectByteStore {
            src: r(4),
            address: 0x2008_8005,
        })
        .unwrap();
        p.push(Instruction48::Idle).unwrap();
        while p.push(Instruction48::Nop).unwrap().0 < 0xd {}
        // 0x9_0012: called through I8 + M8.
        p.push(imm(5, 1)).unwrap();
        p.push(Instruction48::Rts).unwrap();
    });
    let mut program = ProgramMemory::default();
    program.load(&code).unwrap();
    let mut machine = Machine::reset();
    machine.set_pc(0x9_0004);
    let mut memory = data_memory();
    assert_eq!(
        machine.run(&program, &mut memory, None, 100),
        Ok(Stop::Idle)
    );
    assert_eq!(machine.r[5], 1);
    assert_eq!(memory.read_bytes(0x2008_8004, 4).unwrap(), [0, 0xab, 0, 0]);
    let mut words = data_memory();
    assert_eq!(
        tetrad_sharc::machine::Bus::write_byte(&mut words, 0xb_0001, 1),
        Err(BusError::ByteInWordRegion(0xb_0001))
    );
}

/// Regression for review m-owned-dsp F1: PRM `.08.18` scales normal-word offsets
/// and modifiers by 4 for a byte-space pointer; plain MODIFY never scales. The same
/// instructions on a word-space pointer step by one address.
#[test]
fn byte_space_dag_accesses_scale_offsets_and_modifiers() {
    let (i4, i5) = (Dag1Index::new(4).unwrap(), Dag1Index::new(5).unwrap());
    let m2 = Dag1Modify::new(2).unwrap();
    let code = build(0x9_0004, |p| {
        p.push(Instruction48::Immediate {
            dst: UniversalReg::Index(i5),
            value: 0x2008_8000,
        })
        .unwrap();
        p.push(Instruction48::Immediate {
            dst: UniversalReg::Index(i4),
            value: 0xb_0000,
        })
        .unwrap();
        p.push(Instruction48::Immediate {
            dst: UniversalReg::Modify(m2),
            value: 1,
        })
        .unwrap();
        p.push(imm(0, 0x11)).unwrap();
        for index in [i5, i4] {
            p.push(Instruction48::DmStore {
                src: r(0),
                index,
                modifier: m2,
                access: DmAccess::PostModify,
            })
            .unwrap();
            p.push(Instruction48::DmOffsetStore {
                src: r(0),
                index,
                offset: 2,
            })
            .unwrap();
        }
        p.push(Instruction48::ModifyBytes {
            index: i5,
            bytes: 4,
        })
        .unwrap();
        p.push(Instruction48::DmOffsetLoad {
            dst: r(1),
            index: i5,
            offset: 2,
        })
        .unwrap();
        p.push(Instruction48::Idle).unwrap();
    });
    let mut program = ProgramMemory::default();
    program.load(&code).unwrap();
    let mut machine = Machine::reset();
    machine.set_pc(0x9_0004);
    let mut memory = data_memory();
    memory.load_bytes(0x2008_8010, &[0x22, 0, 0, 0]).unwrap();
    assert_eq!(
        machine.run(&program, &mut memory, None, 100),
        Ok(Stop::Idle)
    );
    // Byte space: post-modify by 1 word = +4 bytes, offset 2 words = +8 bytes.
    assert_eq!(machine.i[5], 0x2008_8008);
    assert_eq!(memory.peek(0x2008_8000), Ok(0x11));
    assert_eq!(memory.peek(0x2008_800c), Ok(0x11));
    assert_eq!(memory.peek(0x2008_8004), Ok(0));
    // MODIFY added 4 bytes unscaled; DM(2, I5) then read byte 0x2008_8010.
    assert_eq!(machine.r[1], 0x22);
    // Word space: the same forms step by one address.
    assert_eq!(machine.i[4], 0xb_0001);
    assert_eq!(
        (memory.peek(0xb_0000), memory.peek(0xb_0003)),
        (Ok(0x11), Ok(0x11))
    );
    // Regions must agree with the space the DAG infers from the address.
    let mut map = MemoryMap::default();
    assert_eq!(
        map.add(0x2008_8000, AddressUnit::Word, 4),
        Err(RegionError::Space(0x2008_8000))
    );
    assert_eq!(
        map.add(0x23_fffe, AddressUnit::Word, 4),
        Err(RegionError::Space(0x23_fffe))
    );
}

/// Regression for review w4-dsp F1, in the stock C frame shape. Volume process
/// `0x034`: `I7=MODIFY(I7,-0xa)(NW)` after the two CJUMP pushes, then frame
/// stores down to `DM(-0xb,I6)` (`0x049`). They stay above I7 only if the NW
/// form moves ten words. Volume `0x1b0` `I5=MODIFY(I4,0x24)` (plain; here
/// self-index, as noisegate `0x057`) is a byte offset into the state. In word
/// space both forms add the raw value.
#[test]
fn modify_forms_follow_the_stock_c_frame() {
    let [i3, i4, i6, i7] = [3, 4, 6, 7].map(|n| Dag1Index::new(n).unwrap());
    let reserve = Instruction48::ModifyWords {
        index: i7,
        words: -0xa,
    };
    // The encoder gives the stock words of both forms.
    assert_eq!(reserve.encode().unwrap().bits(), 0x1587_ffff_fff6);
    let plain = Instruction48::ModifyBytes {
        index: i4,
        bytes: 0x2c,
    };
    assert_eq!(plain.encode().unwrap().bits(), 0x1604_0000_002c);
    let code = build(0x9_0004, |p| {
        for instruction in [
            reserve,
            Instruction48::DmOffsetStore {
                src: r(0),
                index: i6,
                offset: -0xb,
            },
            Instruction48::ModifyBytes {
                index: i4,
                bytes: 0x24,
            },
            Instruction48::DmOffsetLoad {
                dst: r(1),
                index: i4,
                offset: 0,
            },
            Instruction48::ModifyWords {
                index: i3,
                words: 3,
            },
            Instruction48::ModifyBytes {
                index: i3,
                bytes: 2,
            },
            Instruction48::Idle,
        ] {
            p.push(instruction).unwrap();
        }
    });
    let mut program = ProgramMemory::default();
    program.load(&code).unwrap();
    let mut memory = data_memory();
    // Stock I7 starts at byte alias 0x24a7f0 (k1 0x16e211..0x16e220).
    let frame = 0x24_a7f0;
    memory.add(0x24_a700, AddressUnit::Byte, 0x40).unwrap();
    memory.load_bytes(0x2008_8024, &[0x55, 0, 0, 0]).unwrap();
    let mut machine = Machine::reset();
    (machine.i[3], machine.i[4]) = (0xb_0000, 0x2008_8000);
    (machine.i[6], machine.i[7]) = (frame, frame - 8);
    machine.r[0] = 0x77;
    machine.set_pc(0x9_0004);
    assert_eq!(machine.run(&program, &mut memory, None, 20), Ok(Stop::Idle));
    // Two pushed words plus ten reserved words, all in bytes.
    assert_eq!(machine.i[7], frame - 12 * 4);
    let lowest = frame - 0xb * 4;
    assert_eq!(memory.peek(lowest), Ok(0x77));
    assert!(lowest > machine.i[7], "frame store below the stack pointer");
    // State byte 36.
    assert_eq!((machine.i[4], machine.r[1]), (0x2008_8024, 0x55));
    assert_eq!(machine.i[3], 0xb_0005);
}

/// Regression for review F2: with ALUSAT clear (the only mode both models
/// accept), PRM 23.15 returns all ones for NaN, infinity and any |x| >= 2^31.
#[test]
fn fix_without_saturation_returns_all_ones_on_overflow() {
    let code = build(0x9_0004, |p| {
        p.push(Instruction48::FixToInteger {
            dst: r(1),
            src: f(0),
        })
        .unwrap();
        p.push(Instruction48::Idle).unwrap();
    });
    let mut program = ProgramMemory::default();
    program.load(&code).unwrap();
    let largest_below = 2_147_483_520.0f32;
    for (input, expected) in [
        (largest_below, 0x7fff_ff80),
        (-largest_below, 0x8000_0080),
        (2_147_483_648.0, u32::MAX),
        (-2_147_483_648.0, u32::MAX),
        (f32::INFINITY, u32::MAX),
        (f32::NEG_INFINITY, u32::MAX),
        (f32::NAN, u32::MAX),
        (2.5, 2),
        (-0.25, 0),
    ] {
        let mut machine = Machine::reset();
        machine.set_pc(0x9_0004);
        machine.r[0] = input.to_bits();
        assert_eq!(
            machine.run(&program, &mut data_memory(), None, 10),
            Ok(Stop::Idle)
        );
        assert_eq!(machine.r[1], expected, "FIX {input}");
    }
}

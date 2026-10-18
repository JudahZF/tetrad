//! VISA shim forms: PRM bit layouts, qc-asm cross-check and execution in the
//! resident machine, including the ISA/VISA switch on CALL and return.
use tetrad_sharc::address::{PmWordAddress24, VisaParcelOffset};
use tetrad_sharc::emit::IsaProgram;
use tetrad_sharc::isa48::{C_RETURN, Instruction48, MODE1_IRPTEN};
use tetrad_sharc::machine::{
    AddressUnit, Bus, InstructionSpace, InterruptBit, Machine, MachineError, MemoryMap,
    ProgramError, ProgramMemory, Stop,
};
use tetrad_sharc::register::{Dag1Index, Dag2Index, DataReg, UniversalReg};
use tetrad_sharc::visa::{VisaCode, VisaInstruction as V};

const SHIM: u32 = 0x1c_0000;
const BODY: u32 = 0x0e_0000;
const DM: u32 = 0x2_0000;

/// qcdsp uses these PRM short forms outside the original owned-block shim.
/// Literal vectors keep this decoder check independent of either emitter.
#[test]
fn general_short_transfers_and_mixed_width_offsets_execute() {
    let parcels = [
        0x707e, 0x0a3f, // 5b I4=R4
        0x9951, // 3c R1=DM(I4,M5), no address change (M5=0)
        0x9971, // 3c DM(I4,M5)=R1
        0x9908, 0x0011, // 15b DM(17,I4)=R0
        0x9808, 0x0a11, // 15b I4=DM(17,I4)
        0xa804, 0, 64, // 15a R4=DM(64,I4)
        0xa918, 0, 64, // DAG2 I8 is a legal source for a DAG1 store
    ];
    let mut program = ProgramMemory::default();
    program.load_visa(SHIM, &parcels).unwrap();
    let mut memory = dm();
    memory.write(DM, 0x3f00_0000).unwrap();
    memory.write(DM + 65, 0x1234_5678).unwrap();
    let mut machine = Machine::reset();
    machine.r[4] = DM;
    machine.r[0] = DM + 1;
    machine.i[8] = 0x8765_4321;
    machine.m[5] = 0;
    machine.set_pc(SHIM);
    assert_eq!(
        machine.run(&program, &mut memory, Some(SHIM + parcels.len() as u32), 20),
        Ok(Stop::Breakpoint(SHIM + parcels.len() as u32))
    );
    assert_eq!(machine.r[1], 0x3f00_0000);
    assert_eq!(machine.r[4], 0x1234_5678);
    assert_eq!(machine.i[4], DM + 1);
    assert_eq!(memory.peek(DM + 65), Ok(0x8765_4321));
}

#[test]
fn qcdsp_float_forms_keep_prm_zero_nan_and_flush_rules() {
    // Type 2a MIN F3=F1,F2; NEG F4=F1; PASS F5=F1.
    let parcels = [
        0x013e, 0x000e, 0x1312, 0x013e, 0x000a, 0x2410, 0x013e, 0x000a, 0x1510,
    ];
    let mut program = ProgramMemory::default();
    program.load_visa(SHIM, &parcels).unwrap();
    for (x, y, minimum, negative, pass) in [
        (0, 0x8000_0000, 0x8000_0000, 0x8000_0000, 0),
        (
            0xbf80_0000,
            0x3f80_0000,
            0xbf80_0000,
            0x3f80_0000,
            0xbf80_0000,
        ),
        (1, 0x3f80_0000, 0, 0x8000_0000, 0),
        (0x7fc0_0000, 0, u32::MAX, u32::MAX, u32::MAX),
    ] {
        let mut machine = Machine::reset();
        machine.arithmetic = tetrad_sharc::machine::Arithmetic::Prm;
        machine.r[1] = x;
        machine.r[2] = y;
        machine.set_pc(SHIM);
        assert_eq!(
            machine.run(&program, &mut dm(), Some(SHIM + 9), 10),
            Ok(Stop::Breakpoint(SHIM + 9))
        );
        assert_eq!(
            [machine.r[3], machine.r[4], machine.r[5]],
            [minimum, negative, pass]
        );
        assert_eq!(machine.stkyx & 0x20 != 0, x == 0x7fc0_0000);
    }
}

fn r(n: u8) -> DataReg {
    DataReg::new(n).unwrap()
}
fn i(n: u8) -> Dag1Index {
    Dag1Index::new(n).unwrap()
}
fn pm(address: u32) -> PmWordAddress24 {
    PmWordAddress24::new(address).unwrap()
}
fn qc_reg(code: u8) -> qc_asm::UniversalRegister {
    qc_asm::UniversalRegister::new(code).unwrap()
}
fn qc_index(n: u8) -> qc_asm::IndexRegister {
    qc_asm::IndexRegister::new(n).unwrap()
}
fn dm() -> MemoryMap {
    let mut memory = MemoryMap::default();
    memory.add(DM, AddressUnit::Word, 0x400).unwrap();
    memory
}
fn visa(origin: u32, code: &[V]) -> ProgramMemory {
    let mut program = ProgramMemory::default();
    load(&mut program, origin, code);
    program
}
fn load(program: &mut ProgramMemory, origin: u32, code: &[V]) {
    let mut stream = VisaCode::default();
    for &instruction in code {
        stream.push(instruction).unwrap();
    }
    program.load_visa(origin, stream.parcels()).unwrap();
}

/// Hand-assembled from the PRM figures; the field split is in each comment.
/// `Some` rows are also checked against qc-asm (independent encoder, its
/// return sequence is stock-identical). The stub rows equal the stock wrapper
/// (volume `0x004..0x008`).
#[test]
fn shim_forms_encode_to_prm_layouts() {
    let i12 = UniversalReg::Dag2Index(Dag2Index::new(12).unwrap());
    let qc_store = |src, index, offset| qc_asm::VisaInstruction::StoreDmOffset {
        src: qc_reg(src),
        index: qc_index(index),
        offset,
    };
    let qc_load = |dst, index, offset| qc_asm::VisaInstruction::LoadDmOffset {
        dst: qc_reg(dst),
        index: qc_index(index),
        offset,
    };
    let vectors = [
        // 17a `.16.14`: 0x0f | UREG 38:32 | DATA 31:0.
        (
            V::Immediate {
                dst: i12,
                value: 0x00f8_0034,
            },
            vec![0x0f1c, 0x00f8, 0x0034],
            Some(qc_asm::VisaInstruction::LoadImmediate {
                dst: qc_reg(0x1c),
                value: 0x00f8_0034,
            }),
        ),
        // 15b `.16.11`: 1001 | I | D | L=0 0 G=0 0 | 1000 | 00 | UREG | DATA7.
        (
            V::DmOffsetStore {
                src: r(15),
                index: i(6),
                offset: -2,
            },
            vec![0x9d08, 0x07fe],
            Some(qc_store(15, 6, -2)),
        ),
        (
            V::DmOffsetLoad {
                dst: r(15),
                index: i(6),
                offset: -2,
            },
            vec![0x9c08, 0x07fe],
            Some(qc_load(15, 6, -2)),
        ),
        (
            V::DmOffsetStore {
                src: r(0),
                index: i(4),
                offset: 63,
            },
            vec![0x9908, 0x003f],
            Some(qc_store(0, 4, 63)),
        ),
        (
            V::DmOffsetStore {
                src: r(0),
                index: i(4),
                offset: -64,
            },
            vec![0x9908, 0x0040],
            Some(qc_store(0, 4, -64)),
        ),
        // 15a `.16.10` past DATA7: 101 G=0 | I | D | L=0 | UREG | ADDR32.
        (
            V::DmOffsetStore {
                src: r(2),
                index: i(4),
                offset: 64,
            },
            vec![0xa902, 0x0000, 0x0040],
            Some(qc_store(2, 4, 64)),
        ),
        (
            V::DmOffsetLoad {
                dst: r(2),
                index: i(4),
                offset: -65,
            },
            vec![0xa802, 0xffff, 0xffbf],
            Some(qc_load(2, 4, -65)),
        ),
        // 19a `.17.04`: 0x15 | G=0 1 W=1 | I | DATA32 (the frame reserve).
        (
            V::ModifyWords {
                index: i(7),
                words: -2,
            },
            vec![0x1587, 0xffff, 0xfffe],
            Some(qc_asm::VisaInstruction::ModifyIndex {
                index: qc_index(7),
                offset: -2,
            }),
        ),
        // 25a: 00011000 | J=1 (DB) at 34 | ADDR24.
        (
            V::CJump(pm(BODY + 0x10)),
            vec![0x1804, 0x000e, 0x0010],
            None,
        ),
        // 3c: 1001 | I=7 | M=7 | D=1 | 1 | R2.
        (V::Push(r(2)), vec![0x9ff2], None),
        // 16a: 100 1 | I=7 | M=7 | G=0 | DATA32 (kind-7 return literal).
        (
            V::PushImmediate(0x00f8_000a),
            vec![0x9fc0, 0x00f8, 0x000a],
            None,
        ),
        // 21c `.17.16`.
        (V::Nop, vec![0x0001], None),
        // 3b `.14.15` I12=DM(M7,I6), 9b `.15.09` JUMP(M14,I12)(DB), 25c, 21c.
        (
            V::CReturn,
            vec![0x4dfe, 0x0e3f, 0x083f, 0x343f, 0x1901, 0x0001],
            Some(qc_asm::VisaInstruction::LeafReturn),
        ),
    ];
    let mut stream = VisaCode::default();
    let mut expected_stream = Vec::new();
    for (instruction, parcels, oracle) in vectors {
        assert_eq!(instruction.parcels().unwrap(), parcels, "{instruction:?}");
        if let Some(oracle) = oracle {
            assert_eq!(oracle.parcels().unwrap(), parcels, "qc-asm {oracle:?}");
        }
        let start = VisaParcelOffset(expected_stream.len() as u32);
        assert_eq!(stream.push(instruction).unwrap(), start);
        expected_stream.extend(parcels);
    }
    assert_eq!(stream.parcels(), expected_stream);
    // Section 2: each parcel little-endian, high parcel first.
    assert_eq!(
        stream.to_le_bytes()[..6],
        [0x1c, 0x0f, 0xf8, 0x00, 0x34, 0x00]
    );
}

/// Every non-branch form runs from VISA space with its own parcel length,
/// and the cycle count shows one step per instruction.
#[test]
fn shim_forms_execute_in_visa_space() {
    let one = 1.0f32.to_bits();
    let program = visa(
        SHIM,
        &[
            V::Immediate {
                dst: UniversalReg::Data(r(0)),
                value: one,
            },
            V::Immediate {
                dst: UniversalReg::Index(i(4)),
                value: DM + 0x10,
            },
            V::DmOffsetStore {
                src: r(0),
                index: i(4),
                offset: 63,
            },
            V::DmOffsetStore {
                src: r(0),
                index: i(4),
                offset: 64,
            },
            V::DmOffsetLoad {
                dst: r(1),
                index: i(4),
                offset: 63,
            },
            V::DmOffsetLoad {
                dst: r(2),
                index: i(4),
                offset: 64,
            },
            V::ModifyWords {
                index: i(4),
                words: -16,
            },
            V::Push(r(0)),
            V::PushImmediate(7),
            V::Nop,
        ],
    );
    // Parcel lengths of the ten instructions above.
    let end = SHIM + [3, 3, 2, 3, 2, 3, 3, 1, 3, 1].iter().sum::<u32>();
    let mut memory = dm();
    let mut machine = Machine::reset();
    (machine.i[7], machine.m[7]) = (DM + 0x200, -1);
    machine.set_pc(SHIM);
    assert_eq!(
        machine.run(&program, &mut memory, Some(end), 20),
        Ok(Stop::Breakpoint(end))
    );
    assert_eq!([machine.r[0], machine.r[1], machine.r[2]], [one; 3]);
    assert_eq!(memory.peek(DM + 0x10 + 63), Ok(one));
    assert_eq!(memory.peek(DM + 0x10 + 64), Ok(one));
    assert_eq!(machine.i[4], DM);
    assert_eq!(memory.peek(DM + 0x200), Ok(one));
    assert_eq!(memory.peek(DM + 0x1ff), Ok(7));
    assert_eq!(machine.i[7], DM + 0x1fe);
    assert_eq!(machine.cycles(), 10);
}

/// The owned stub (stock wrapper shape, volume `0x004`): the kernel's C call
/// lands on `CJUMP body (DB)` and its two pushes. The ISA body returns with
/// the 48-bit C return (fortin section 3 `0xdf..0xe2`) to the stub's C
/// epilogue, which returns to the kernel. The kernel's own CJUMP is applied
/// by hand (R2 = I6, I6 = I7, then its two `DM(I7, M7)` pushes).
#[test]
fn stub_cjumps_into_isa_and_returns_through_both_c_returns() {
    let mut program = visa(
        SHIM,
        &[
            V::CJump(pm(BODY)),
            V::Push(r(2)),
            // Return literal: the epilogue (parcel 7) − 1.
            V::PushImmediate(SHIM + 6),
            V::CReturn,
        ],
    );
    // The body stores (marker, R8) through R4.
    let mut body = IsaProgram::new(pm(BODY));
    for instruction in [
        Instruction48::RegisterMove {
            dst: UniversalReg::Index(i(4)),
            src: UniversalReg::Data(r(4)),
        },
        Instruction48::Immediate {
            dst: UniversalReg::Data(r(0)),
            value: 0x1234_5678,
        },
        Instruction48::DmOffsetStore {
            src: r(0),
            index: i(4),
            offset: 0,
        },
        Instruction48::DmOffsetStore {
            src: r(8),
            index: i(4),
            offset: 1,
        },
    ]
    .into_iter()
    .chain(C_RETURN)
    {
        body.push(instruction).unwrap();
    }
    program.load(&body.finish().unwrap()).unwrap();

    let (caller_frame, caller_stack, state, ret) = (DM + 0x3f8, DM + 0x3f0, DM + 0x100, 0x9_0100);
    let mut memory = dm();
    let mut machine = Machine::reset();
    // C run-time constants; arguments R4 = state, R8 = io.
    (machine.m[7], machine.m[14]) = (-1, 1);
    (machine.r[4], machine.r[8]) = (state, 0xdead);
    (machine.i[6], machine.i[7]) = (caller_frame, caller_stack);
    machine.r[2] = machine.i[6];
    machine.i[6] = machine.i[7];
    memory.write(machine.i[7], machine.r[2]).unwrap();
    memory.write(machine.i[7] - 1, ret).unwrap();
    machine.i[7] -= 2;
    machine.set_pc(SHIM);

    assert_eq!(InstructionSpace::of(SHIM), InstructionSpace::Visa);
    assert_eq!(InstructionSpace::of(BODY), InstructionSpace::Isa);
    // The epilogue jumps to the saved address + M14, in ISA space.
    assert_eq!(
        machine.run(&program, &mut memory, Some(ret + 1), 40),
        Ok(Stop::Breakpoint(ret + 1))
    );
    assert_eq!(memory.peek(state), Ok(0x1234_5678));
    assert_eq!(memory.peek(state + 1), Ok(0xdead));
    // The stub frame: the kernel frame pointer, then the return literal.
    assert_eq!(memory.peek(caller_stack - 2), Ok(caller_stack));
    assert_eq!(memory.peek(caller_stack - 3), Ok(SHIM + 6));
    assert_eq!((machine.i[6], machine.i[7]), (caller_frame, caller_stack));
    assert_eq!(machine.pc_stack_depth(), 0);
    // Stub 3 + body 4 + ISA return 4 + VISA return 4 (delay slots run once).
    assert_eq!(machine.cycles(), 15);
}

/// PRM `.06.026`: a delayed branch and its two slots are atomic and the slots
/// may not branch.
#[test]
fn delayed_jump_holds_interrupts_and_refuses_slot_branches() {
    // `JUMP (M14, I12) (DB)` is parcels 2..4 of the C epilogue.
    let jump = &V::CReturn.parcels().unwrap()[2..4];
    let nop = V::Nop.parcels().unwrap();
    // Type 11c RTS (PRM `.15.18`): 0000101 X=0 | 1 | J=0 | COND=31 | LR=0.
    let rts = [0x0abe];
    let target = 0x9_0000;
    let start = |slots: &[&[u16]]| {
        let mut parcels = jump.to_vec();
        parcels.extend(slots.concat());
        let mut program = ProgramMemory::default();
        program.load_visa(SHIM, &parcels).unwrap();
        let mut machine = Machine::reset();
        (machine.i[12], machine.m[14]) = (target, 0);
        machine.set_pc(SHIM);
        (program, machine)
    };
    let mut memory = dm();

    let (program, mut machine) = start(&[&nop, &nop]);
    machine.mode1 |= MODE1_IRPTEN;
    machine.imask = 1 << InterruptBit::SECI.get();
    machine.step(&program, &mut memory).unwrap();
    machine.raise(InterruptBit::SECI);
    assert_eq!(
        machine.run(&program, &mut memory, Some(target), 10),
        Ok(Stop::Breakpoint(target))
    );
    assert_eq!(machine.irptl, 1 << InterruptBit::SECI.get());
    assert_eq!(machine.imaskp, 0);
    assert_eq!(machine.cycles(), 3);

    let (program, mut machine) = start(&[&rts]);
    assert_eq!(
        machine.run(&program, &mut memory, None, 10),
        Err(MachineError::BranchInDelaySlot(SHIM + 2))
    );
}

/// Code must sit wholly in the space that decodes it.
#[test]
fn code_space_follows_the_fetch_address() {
    for (pc, space) in [
        (0x11_ffff, InstructionSpace::Isa),
        (0x12_0000, InstructionSpace::Visa),
        (0x1c_ffff, InstructionSpace::Visa),
        (0x1d_0000, InstructionSpace::Isa),
        (0xba_0000, InstructionSpace::Visa),
    ] {
        assert_eq!(InstructionSpace::of(pc), space, "{pc:#x}");
    }
    let mut program = ProgramMemory::default();
    let mut isa = IsaProgram::new(pm(SHIM));
    isa.push(Instruction48::Nop).unwrap();
    assert_eq!(
        program.load(&isa.finish().unwrap()),
        Err(ProgramError::Space {
            base: SHIM,
            space: InstructionSpace::Isa
        })
    );
    for base in [BODY, 0x1c_fffe] {
        assert_eq!(
            program.load_visa(base, &[0x0001; 3]),
            Err(ProgramError::Space {
                base,
                space: InstructionSpace::Visa
            })
        );
    }
}

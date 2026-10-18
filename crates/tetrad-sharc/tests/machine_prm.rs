//! `Machine` under `Arithmetic::Prm`: 40-bit data registers and the AIS/MIS
//! stickies that the stock kernel's NaN guard tests after each process call.
use tetrad_sharc::address::{DmWordAddress32, PmWordAddress24};
use tetrad_sharc::emit::IsaProgram;
use tetrad_sharc::isa48::{Instruction48, MODE1_RND32, Mode1Mask};
use tetrad_sharc::machine::{
    AddressUnit, Arithmetic, Machine, MemoryMap, ProgramMemory, STKYX_AIS, STKYX_MIS, Stop,
};
use tetrad_sharc::register::{DataReg, FloatReg, UniversalReg};

const ORIGIN: u32 = 0x9_0000;
const WORD: u32 = 0xb_0000;

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

/// Runs `code` to its end from `machine`'s registers.
fn run(machine: &mut Machine, memory: &mut MemoryMap, code: &[Instruction48]) {
    let mut program = IsaProgram::new(PmWordAddress24::new(ORIGIN).unwrap());
    for &instruction in code {
        program.push(instruction).unwrap();
    }
    let mut loaded = ProgramMemory::default();
    loaded.load(&program.finish().unwrap()).unwrap();
    machine.set_pc(ORIGIN);
    let end = ORIGIN + code.len() as u32;
    assert_eq!(
        machine.run(&loaded, memory, Some(end), 100),
        Ok(Stop::Breakpoint(end))
    );
}

/// PRM 4.06: computes keep bits 7:0 (RND32 clear keeps a 32-bit significand,
/// FLOAT always does), data register moves carry them, and 32-bit transfers
/// (stores, loads, fixed-point results) drop them. Under RND32 an operand
/// ignores them, so `FIX(FLOAT 2^24 + 1)` truncates to 2^24.
#[test]
fn prm_registers_keep_40_bits_until_a_32_bit_transfer() {
    let mut memory = MemoryMap::default();
    memory.add(WORD, AddressUnit::Word, 1).unwrap();
    let mut machine = Machine::reset();
    machine.arithmetic = Arithmetic::Prm;
    machine.mode1 = 0;
    run(
        &mut machine,
        &mut memory,
        &[
            imm(0, 0),
            imm(1, (1 << 24) + 1),
            Instruction48::FloatFromInteger {
                dst: f(2),
                src: r(1),
            },
            Instruction48::RegisterMove {
                dst: UniversalReg::Data(r(3)),
                src: UniversalReg::Data(r(2)),
            },
            Instruction48::FloatAdd {
                dst: f(4),
                lhs: f(3),
                rhs: f(0),
            },
            Instruction48::DmDirectStore {
                src: r(4),
                address: DmWordAddress32::new(WORD),
            },
            Instruction48::DmDirectLoad {
                dst: r(9),
                address: DmWordAddress32::new(WORD),
            },
            Instruction48::IntegerAdd {
                dst: r(10),
                lhs: r(3),
                rhs: r(0),
            },
            Instruction48::Mode1Set(Mode1Mask::new(MODE1_RND32).unwrap()),
            Instruction48::FloatAdd {
                dst: f(5),
                lhs: f(3),
                rhs: f(0),
            },
            Instruction48::FixToInteger {
                dst: r(6),
                src: f(3),
            },
        ],
    );
    // 2^24 + 1 needs 25 significand bits: the last one is extension bit 7.
    let two_24 = 16_777_216.0_f32.to_bits();
    assert_eq!((machine.r[2], machine.extension[2]), (two_24, 0x80));
    assert_eq!((machine.r[3], machine.extension[3]), (two_24, 0x80));
    assert_eq!((machine.r[4], machine.extension[4]), (two_24, 0x80));
    assert_eq!(memory.peek(WORD), Ok(two_24));
    assert_eq!(machine.extension[9], 0);
    assert_eq!((machine.r[10], machine.extension[10]), (two_24, 0));
    assert_eq!((machine.r[5], machine.extension[5]), (two_24, 0));
    assert_eq!(machine.r[6], 1 << 24);

    // Host f32 has no extension: the same FLOAT rounds to nearest even.
    let mut host = Machine::reset();
    host.mode1 = 0;
    run(
        &mut host,
        &mut memory,
        &[
            imm(1, (1 << 24) + 3),
            Instruction48::FloatFromInteger {
                dst: f(2),
                src: r(1),
            },
        ],
    );
    assert_eq!(
        (host.r[2], host.extension[2]),
        (16_777_220.0_f32.to_bits(), 0)
    );
}

/// The kernel faults a slot when AIS or MIS is set after process (ABI record
/// §3, NaN guard). Invalid operations and NaN operands set them; overflow
/// (AV) does not. Compares read flushed operands. Host f32 sets no sticky.
#[test]
fn prm_invalid_operations_set_ais_or_mis_and_overflow_does_not() {
    let infinity = f32::INFINITY.to_bits();
    let nan = f32::NAN.to_bits();
    let one = 1.0_f32.to_bits();
    let multiply = Instruction48::FloatMultiply {
        dst: f(0),
        lhs: f(1),
        rhs: f(2),
    };
    let add = Instruction48::FloatAdd {
        dst: f(0),
        lhs: f(1),
        rhs: f(2),
    };
    let subtract = Instruction48::FloatSubtract {
        dst: f(0),
        lhs: f(1),
        rhs: f(2),
    };
    let compare = Instruction48::FloatCompare {
        lhs: f(1),
        rhs: f(2),
    };
    let fix = Instruction48::FixToInteger {
        dst: r(0),
        src: f(1),
    };
    let cases = [
        (multiply, 0, infinity, STKYX_MIS),
        (multiply, nan, one, STKYX_MIS),
        (add, infinity, infinity | 1 << 31, STKYX_AIS),
        (subtract, infinity, infinity, STKYX_AIS),
        (compare, nan, one, STKYX_AIS),
        (fix, infinity, 0, STKYX_AIS),
        (multiply, f32::MAX.to_bits(), 2.0_f32.to_bits(), 0),
        (fix, 2.0_f32.powi(40).to_bits(), 0, 0),
        (add, one, 1, 0),
    ];
    let mut memory = MemoryMap::default();
    for (instruction, x, y, sticky) in cases {
        let mut machine = Machine::reset();
        machine.arithmetic = Arithmetic::Prm;
        run(
            &mut machine,
            &mut memory,
            &[imm(1, x), imm(2, y), instruction],
        );
        assert_eq!(machine.stkyx, sticky, "{instruction:?}({x:#x}, {y:#x})");
    }

    // COMP(denormal, 0): equal under the operand rule, less-than in host f32.
    let mut prm = Machine::reset();
    prm.arithmetic = Arithmetic::Prm;
    let mut host = Machine::reset();
    for machine in [&mut prm, &mut host] {
        run(
            machine,
            &mut memory,
            &[imm(1, 1), imm(2, 0), compare, multiply],
        );
    }
    assert!(prm.flags.zero && !host.flags.zero);
    assert_eq!(host.stkyx, 0);
}

use tetrad_sharc::address::{DmWordAddress32, PcRelative24, PmWordAddress24};
use tetrad_sharc::isa48::{
    BitOperation, BranchKind, BranchTarget, Condition, EncodeError, IMASK_EMUI, IMASK_RSTI,
    IMASK_SECI, IMASK_WRITABLE, Instruction48, InstructionWord48, InterruptMask, MODE1_CBUFEN,
    MODE1_IRPTEN, MODE1_NESTM, MODE1_PEYEN, MODE1_RND32, MODE1_SCALAR_CLEAR, MODE2_CADIS,
    MODE2_SNEN, Mmask, Mode1Mask, Mode2Mask, STKYX_LSEM, STKYX_PCEM, STKYX_SSEM, StackEmptyMask,
    StackPops, SystemBitMask, UnsupportedFeature,
};
use tetrad_sharc::register::DataReg;

// Invented report vectors only. RTI and MMASK immediate use the PRM/sparse-raw
// gate; the oracle's erroneous bit39 and legacy UREG encodings are not authority.
#[test]
fn supported_resident_vectors_have_exact_words_and_six_byte_le() {
    let bit = |operation, operand| Instruction48::SystemBit { operation, operand };
    let mode1 = |bits| SystemBitMask::Mode1(Mode1Mask::new(bits).unwrap());
    let mode2 = |bits| SystemBitMask::Mode2(Mode2Mask::new(bits).unwrap());
    let imask = |bits| SystemBitMask::Interrupt(InterruptMask::new(bits).unwrap());
    let empty = |bits| SystemBitMask::StackEmpty(StackEmptyMask::new(bits).unwrap());
    let pop = |stacks, flush_conflict_cache| Instruction48::PopStacks {
        stacks,
        flush_conflict_cache,
    };
    let load = |reg, address| Instruction48::DmDirectLoad {
        dst: DataReg::new(reg).unwrap(),
        address: DmWordAddress32::new(address),
    };
    let store = |reg, address| Instruction48::DmDirectStore {
        src: DataReg::new(reg).unwrap(),
        address: DmWordAddress32::new(address),
    };
    let call = |address| Instruction48::Branch {
        kind: BranchKind::Call,
        condition: Condition::Always,
        target: BranchTarget::Absolute(PmWordAddress24::new(address).unwrap()),
    };
    let jump = |condition, offset| Instruction48::Branch {
        kind: BranchKind::Jump,
        condition,
        target: BranchTarget::Relative(PcRelative24::new(offset).unwrap()),
    };
    use BitOperation::{Clear, Set, Test};
    let vectors = [
        (bit(Clear, mode1(0x1000)), 0x142200001000, "001000002214"),
        (bit(Set, mode1(0x10000)), 0x140200010000, "000001000214"),
        (bit(Clear, mode1(0x200800)), 0x142200200800, "000820002214"),
        (bit(Set, mode2(0x10)), 0x140400000010, "100000000414"),
        (bit(Clear, mode2(0x10)), 0x142400000010, "100000002414"),
        (bit(Clear, imask(0x8000)), 0x142b00008000, "008000002b14"),
        (bit(Set, imask(0x8000)), 0x140b00008000, "008000000b14"),
        (
            bit(Set, SystemBitMask::Mmask(Mmask::new(0x200000).unwrap())),
            0x140300200000,
            "000020000314",
        ),
        (bit(Test, empty(0x400000)), 0x148800400000, "000040008814"),
        (bit(Test, empty(0x1000000)), 0x148801000000, "000000018814"),
        (bit(Test, empty(0x4000000)), 0x148804000000, "000000048814"),
        (pop(StackPops::Loop, false), 0x174000000000, "000000004017"),
        (
            pop(StackPops::Status, false),
            0x171000000000,
            "000000001017",
        ),
        (pop(StackPops::Pc, false), 0x170400000000, "000000000417"),
        (pop(StackPops::All, false), 0x175400000000, "000000005417"),
        (
            Instruction48::FlushConflictCache,
            0x170200000000,
            "000000000217",
        ),
        (pop(StackPops::Pc, true), 0x170600000000, "000000000617"),
        (load(7, 0x12345678), 0x100712345678, "785634120710"),
        (store(7, 0x12345678), 0x110712345678, "785634120711"),
        (load(0, 0xffffffff), 0x1000ffffffff, "ffffffff0010"),
        (store(15, 0), 0x110f00000000, "000000000f11"),
        (load(3, 0x30024), 0x100300030024, "240003000310"),
        (store(3, 0x30024), 0x110300030024, "240003000311"),
        (Instruction48::Rti, 0x0b3e00000000, "000000003e0b"),
        (Instruction48::Rts, 0x0a3e00000000, "000000003e0a"),
        (call(0x123456), 0x06be00123456, "56341200be06"),
        (call(0xffffff), 0x06be00ffffff, "ffffff00be06"),
        (jump(Condition::TestFlag, 3), 0x071a00000003, "030000001a07"),
        (
            jump(Condition::NotTestFlag, -3),
            0x073a00fffffd,
            "fdffff003a07",
        ),
        (
            Instruction48::MmaskImmediate(Mmask::new(0x200000).unwrap()),
            0x0f7300200000,
            "00002000730f",
        ),
    ];
    assert_eq!(vectors.len(), 30);
    for (instruction, bits, le_hex) in vectors {
        let expected: [u8; 6] =
            std::array::from_fn(|n| u8::from_str_radix(&le_hex[n * 2..n * 2 + 2], 16).unwrap());
        let word = instruction.encode().unwrap();
        assert_eq!(word.bits(), bits, "{instruction:?}");
        assert_eq!(word.to_le_bytes(), expected, "{instruction:?}");
        assert_eq!(InstructionWord48::from_le_bytes(expected), word);
    }
}

#[test]
fn checked_masks_reject_every_reserved_or_outside_policy_bit() {
    let mode1 = MODE1_SCALAR_CLEAR | MODE1_RND32 | MODE1_CBUFEN | MODE1_IRPTEN | MODE1_NESTM;
    let mmask = MODE1_SCALAR_CLEAR | MODE1_IRPTEN | MODE1_NESTM;
    let mode2 = MODE2_CADIS | MODE2_SNEN;
    let empty = STKYX_LSEM | STKYX_SSEM | STKYX_PCEM;
    for n in 0..32 {
        let mask = 1 << n;
        assert_eq!(Mode1Mask::new(mask).is_ok(), mask & mode1 != 0);
        assert_eq!(Mode2Mask::new(mask).is_ok(), mask & mode2 != 0);
        assert_eq!(Mmask::new(mask).is_ok(), mask & mmask != 0);
        assert_eq!(InterruptMask::new(mask).is_ok(), mask & IMASK_WRITABLE != 0);
        assert_eq!(StackEmptyMask::new(mask).is_ok(), mask & empty != 0);
    }
    assert!(InterruptMask::new(IMASK_RSTI | IMASK_EMUI).is_err());
    assert!(InterruptMask::new(IMASK_WRITABLE | 4).is_err());
    assert!(Mmask::new(MODE1_RND32).is_err());
    assert!(Mmask::new(MODE1_CBUFEN).is_err());
    assert!(StackEmptyMask::new(1 << 21).is_err()); // PCFL is not an empty bit.
    assert!(StackEmptyMask::new(1 << 25).is_err()); // LSOV is not an empty bit.
    assert_eq!(
        InterruptMask::new(IMASK_WRITABLE).unwrap().bits(),
        IMASK_WRITABLE
    );
}

#[test]
fn read_only_status_and_unsupported_numerical_modes_cannot_be_written() {
    for operation in [BitOperation::Set, BitOperation::Clear] {
        assert_eq!(
            Instruction48::SystemBit {
                operation,
                operand: SystemBitMask::StackEmpty(StackEmptyMask::new(STKYX_PCEM).unwrap()),
            }
            .encode(),
            Err(EncodeError::ReadOnlyStackStatus)
        );
    }
    for bit in [1, 2, 1 << 13, 1 << 15, 1 << 22, 1 << 23] {
        let mask = Mode1Mask::new(bit).unwrap();
        assert_eq!(
            Instruction48::SystemBit {
                operation: BitOperation::Set,
                operand: SystemBitMask::Mode1(mask),
            }
            .encode(),
            Err(EncodeError::UnsupportedMode1Set(bit))
        );
        assert!(
            Instruction48::SystemBit {
                operation: BitOperation::Test,
                operand: SystemBitMask::Mode1(mask),
            }
            .encode()
            .is_ok()
        );
    }
    assert_eq!(
        Instruction48::SystemBit {
            operation: BitOperation::Set,
            operand: SystemBitMask::Mode1(Mode1Mask::new(MODE1_PEYEN).unwrap()),
        }
        .encode(),
        Err(EncodeError::Unsupported(UnsupportedFeature::Simd))
    );
}

#[test]
fn sysreg_and_ureg_selectors_and_rti_bit40_are_distinct() {
    let mask = Mmask::new(MODE1_PEYEN).unwrap();
    assert_eq!(
        Instruction48::MmaskImmediate(mask).encode().unwrap().bits() >> 32 & 0x7f,
        0x73
    );
    for operation in [BitOperation::Set, BitOperation::Clear, BitOperation::Test] {
        for (operand, selector) in [
            (
                SystemBitMask::Mode1(Mode1Mask::new(MODE1_RND32).unwrap()),
                2,
            ),
            (SystemBitMask::Mmask(mask), 3),
            (
                SystemBitMask::Mode2(Mode2Mask::new(MODE2_CADIS).unwrap()),
                4,
            ),
            (
                SystemBitMask::Interrupt(InterruptMask::new(IMASK_SECI).unwrap()),
                0xb,
            ),
        ] {
            assert_eq!(
                Instruction48::SystemBit { operation, operand }
                    .encode()
                    .unwrap()
                    .bits()
                    >> 32
                    & 0xf,
                selector
            );
        }
    }
    let rti = Instruction48::Rti.encode().unwrap().bits();
    let rts = Instruction48::Rts.encode().unwrap().bits();
    assert_eq!(rti ^ rts, 1 << 40);
    assert_eq!(rti & (1 << 39), 0);
    assert_ne!(rti, 0x0abe00000000); // Explicitly reject the oracle bug.
}

#[test]
fn stack_selection_and_unverified_maintenance_remain_bounded() {
    for (stacks, fields) in [
        (StackPops::Loop, 0x40),
        (StackPops::Status, 0x10),
        (StackPops::Pc, 4),
        (StackPops::LoopStatus, 0x50),
        (StackPops::LoopPc, 0x44),
        (StackPops::StatusPc, 0x14),
        (StackPops::All, 0x54),
    ] {
        for flush in [false, true] {
            let bits = Instruction48::PopStacks {
                stacks,
                flush_conflict_cache: flush,
            }
            .encode()
            .unwrap()
            .bits();
            assert_eq!(
                bits,
                0x170000000000 | ((fields | if flush { 2 } else { 0 }) << 32)
            );
            assert_eq!(bits & ((1 << 39) | (1 << 37) | (1 << 35) | 0xffff_ffff), 0);
        }
    }
    for feature in [
        UnsupportedFeature::StackPush,
        UnsupportedFeature::MixedStackPushPop,
        UnsupportedFeature::L1InstructionCache,
        UnsupportedFeature::L1DataCache,
        UnsupportedFeature::L1ProgramCache,
        UnsupportedFeature::BranchTargetBuffer,
        UnsupportedFeature::Toggle,
        UnsupportedFeature::DelayedReturn,
        UnsupportedFeature::LoopReturn,
        UnsupportedFeature::SystemRegister,
    ] {
        assert_eq!(
            Instruction48::Unsupported(feature).encode(),
            Err(EncodeError::Unsupported(feature))
        );
    }
}

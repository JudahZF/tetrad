use tetrad_sharc::address::{PcRelative24, PmWordAddress24};
use tetrad_sharc::emit::{BranchRelocation, EmitError, IsaProgram, Label};
use tetrad_sharc::gain::compile_gain;
use tetrad_sharc::isa48::{
    BranchKind, BranchTarget, Condition, DmAccess, EncodeError, Instruction48, InstructionWord48,
    MODE1_PEYEN, Mode1Mask, UnsupportedFeature,
};
use tetrad_sharc::register::{
    Dag1Base, Dag1Index, Dag1Length, Dag1Modify, DataReg, FloatReg, UniversalReg,
};

// Invented instructions from the report: manual-derived words with independent
// assembler section-byte agreement. The excluded PM vector is not a fixture.
#[test]
fn all_19_supported_manual_and_oracle_vectors() {
    let r: [DataReg; 16] = std::array::from_fn(|i| DataReg::new(i as u8).unwrap());
    let f: [FloatReg; 16] = std::array::from_fn(|i| FloatReg::new(i as u8).unwrap());
    let i: [Dag1Index; 8] = std::array::from_fn(|i| Dag1Index::new(i as u8).unwrap());
    let m: [Dag1Modify; 8] = std::array::from_fn(|i| Dag1Modify::new(i as u8).unwrap());
    let vectors = [
        (
            Instruction48::FloatMultiply {
                dst: f[3],
                lhs: f[5],
                rhs: f[9],
            },
            0x013e00130359,
        ),
        (
            Instruction48::FloatAdd {
                dst: f[6],
                lhs: f[2],
                rhs: f[11],
            },
            0x013e0008162b,
        ),
        (
            Instruction48::FloatSubtract {
                dst: f[7],
                lhs: f[13],
                rhs: f[4],
            },
            0x013e000827d4,
        ),
        (
            Instruction48::RegisterMove {
                dst: UniversalReg::Index(i[4]),
                src: UniversalReg::Data(r[1]),
            },
            0x703e8a000000,
        ),
        (
            Instruction48::RegisterMove {
                dst: UniversalReg::Index(i[4]),
                src: UniversalReg::Data(r[2]),
            },
            0x703f0a000000,
        ),
        (
            Instruction48::RegisterMove {
                dst: UniversalReg::Index(i[4]),
                src: UniversalReg::Data(r[4]),
            },
            0x707e0a000000,
        ),
        (
            Instruction48::DmLoad {
                dst: r[7],
                index: i[2],
                modifier: m[5],
                access: DmAccess::PostModify,
            },
            0x557e03800000,
        ),
        (
            Instruction48::DmStore {
                src: r[7],
                index: i[2],
                modifier: m[5],
                access: DmAccess::PostModify,
            },
            0x557e83800000,
        ),
        (
            Instruction48::DmLoad {
                dst: r[7],
                index: i[2],
                modifier: m[5],
                access: DmAccess::PreModify,
            },
            0x457e03800000,
        ),
        (
            Instruction48::DmLoad {
                dst: r[7],
                index: i[2],
                modifier: m[4],
                access: DmAccess::PostModify,
            },
            0x553e03800000,
        ),
        (
            Instruction48::DmLoad {
                dst: r[7],
                index: i[3],
                modifier: m[5],
                access: DmAccess::PostModify,
            },
            0x577e03800000,
        ),
        (
            Instruction48::DmOffsetLoad {
                dst: r[7],
                index: i[2],
                offset: -3,
            },
            0xa407fffffffd,
        ),
        (
            Instruction48::ModifyWords {
                index: i[2],
                words: -1,
            },
            0x1582ffffffff,
        ),
        (
            Instruction48::Mode1Set(Mode1Mask::new(0x10000).unwrap()),
            0x140200010000,
        ),
        (
            Instruction48::Mode1Clear(Mode1Mask::new(0x200000).unwrap()),
            0x142200200000,
        ),
        (Instruction48::Rts, 0x0a3e00000000),
        (
            Instruction48::Branch {
                kind: BranchKind::Call,
                condition: Condition::Always,
                target: BranchTarget::Absolute(PmWordAddress24::new(0x123456).unwrap()),
            },
            0x06be00123456,
        ),
        (
            Instruction48::Branch {
                kind: BranchKind::Jump,
                condition: Condition::NotEqual,
                target: BranchTarget::Relative(PcRelative24::new(-4).unwrap()),
            },
            0x072000fffffc,
        ),
        (
            Instruction48::Immediate {
                dst: UniversalReg::Modify(m[1]),
                value: 0x12345678,
            },
            0x0f2112345678,
        ),
    ];
    assert_eq!(vectors.len(), 19);
    for (instruction, expected) in vectors {
        assert_eq!(
            instruction.encode().unwrap().bits(),
            expected,
            "{instruction:?}"
        );
    }
}

// Manual-derived Type 2a words (ALUOP table, COND=31) for invented operands. Each
// matched independent `selas` section bytes; RECIPS has no variant (seed ROM unknown).
#[test]
fn neural_alu_manual_and_oracle_vectors() {
    let r: [DataReg; 16] = std::array::from_fn(|i| DataReg::new(i as u8).unwrap());
    let f: [FloatReg; 16] = std::array::from_fn(|i| FloatReg::new(i as u8).unwrap());
    for (instruction, word) in [
        (
            Instruction48::FloatAbs {
                dst: f[3],
                src: f[9],
            },
            0x013e000b0390,
        ),
        (
            Instruction48::FloatAbs {
                dst: f[13],
                src: f[2],
            },
            0x013e000b0d20,
        ),
        (
            Instruction48::FloatMax {
                dst: f[3],
                lhs: f[5],
                rhs: f[9],
            },
            0x013e000e2359,
        ),
        (
            Instruction48::FloatClip {
                dst: f[3],
                src: f[5],
                bound: f[9],
            },
            0x013e000e3359,
        ),
        (
            Instruction48::FloatCopySign {
                dst: f[3],
                magnitude: f[5],
                sign: f[9],
            },
            0x013e000e0359,
        ),
        (
            Instruction48::FloatCompare {
                lhs: f[5],
                rhs: f[9],
            },
            0x013e0008a059,
        ),
        (
            Instruction48::FloatCompare {
                lhs: f[14],
                rhs: f[1],
            },
            0x013e0008a0e1,
        ),
        (
            Instruction48::IntegerSubtract {
                dst: r[3],
                lhs: r[5],
                rhs: r[9],
            },
            0x013e00002359,
        ),
    ] {
        assert_eq!(
            instruction.encode().unwrap().bits(),
            word,
            "{instruction:?}"
        );
    }
}

#[test]
fn gain_all_nine_words_and_all_54_little_endian_bytes() {
    let words = [
        0x0f2000000001,
        0x013e00021280,
        0x070000000006,
        0x503e00000000,
        0x013e00130004,
        0x523e80000000,
        0x013e0002a220,
        0x072000fffffc,
        0x0a3e00000000,
    ];
    let bytes = [
        0x01, 0x00, 0x00, 0x00, 0x20, 0x0f, 0x80, 0x12, 0x02, 0x00, 0x3e, 0x01, 0x06, 0x00, 0x00,
        0x00, 0x00, 0x07, 0x00, 0x00, 0x00, 0x00, 0x3e, 0x50, 0x04, 0x00, 0x13, 0x00, 0x3e, 0x01,
        0x00, 0x00, 0x00, 0x80, 0x3e, 0x52, 0x20, 0xa2, 0x02, 0x00, 0x3e, 0x01, 0xfc, 0xff, 0xff,
        0x00, 0x20, 0x07, 0x00, 0x00, 0x00, 0x00, 0x3e, 0x0a,
    ];
    for origin in [0, 0x12000, 0xfffff7] {
        let code = compile_gain(PmWordAddress24::new(origin).unwrap()).unwrap();
        assert_eq!(code.word_count(), 9);
        assert_eq!(
            code.words().iter().map(|w| w.bits()).collect::<Vec<_>>(),
            words
        );
        assert_eq!(code.isa48_le_bytes(), bytes);
        assert_eq!(code.symbols()[&Label::new("gain.entry")].get(), origin);
        assert_eq!(code.symbols()[&Label::new("gain.return")].get(), origin + 8);
    }
}

#[test]
fn word_bounds_and_container_serialization_are_explicit() {
    assert!(InstructionWord48::from_bits(1 << 48).is_err());
    assert!(InstructionWord48::from_bits(u64::MAX).is_err());
    let max = InstructionWord48::from_bits(InstructionWord48::MAX).unwrap();
    assert_eq!(max.to_le_bytes(), [0xff; 6]);
    let word = InstructionWord48::from_bits(0x0f2112345678).unwrap();
    assert_eq!(word.to_le_bytes(), [0x78, 0x56, 0x34, 0x12, 0x21, 0x0f]);
    assert_eq!(InstructionWord48::from_le_bytes(word.to_le_bytes()), word);
    assert_eq!(Instruction48::Nop.encode().unwrap().bits(), 0);
}

#[test]
fn register_classes_enforce_all_bounds() {
    assert!(DataReg::new(15).is_ok());
    assert!(DataReg::new(16).is_err());
    assert!(FloatReg::new(15).is_ok());
    assert!(FloatReg::new(16).is_err());
    for index in [8, 15, 255] {
        assert!(Dag1Index::new(index).is_err());
        assert!(Dag1Modify::new(index).is_err());
        assert!(Dag1Base::new(index).is_err());
        assert!(Dag1Length::new(index).is_err());
    }
    assert!(Dag1Index::new(7).is_ok());
    assert!(Dag1Modify::new(7).is_ok());
    assert!(Dag1Base::new(7).is_ok());
    assert!(Dag1Length::new(7).is_ok());
    assert_eq!(
        DataReg::from(FloatReg::new(15).unwrap()),
        DataReg::new(15).unwrap()
    );
}

#[test]
fn immediate_destinations_and_move_source_split_cover_each_class() {
    let destinations = [
        UniversalReg::Data(DataReg::new(15).unwrap()),
        UniversalReg::Index(Dag1Index::new(7).unwrap()),
        UniversalReg::Modify(Dag1Modify::new(7).unwrap()),
        UniversalReg::Length(Dag1Length::new(7).unwrap()),
        UniversalReg::Base(Dag1Base::new(7).unwrap()),
    ];
    for (dst, expected) in destinations.into_iter().zip([
        0x0f0fffffffff,
        0x0f17ffffffff,
        0x0f27ffffffff,
        0x0f37ffffffff,
        0x0f47ffffffff,
    ]) {
        assert_eq!(
            Instruction48::Immediate {
                dst,
                value: u32::MAX
            }
            .encode()
            .unwrap()
            .bits(),
            expected
        );
    }
    // Manual fields, not additional oracle claims: both low source bits and the
    // full source class matter. B7 source is 0x47, destination L7 is 0x37.
    let transfer = Instruction48::RegisterMove {
        src: UniversalReg::Base(Dag1Base::new(7).unwrap()),
        dst: UniversalReg::Length(Dag1Length::new(7).unwrap()),
    };
    assert_eq!(transfer.encode().unwrap().bits(), 0x747f9b800000);
}

#[test]
fn memory_field_limits_and_signed_offset_store() {
    let r = DataReg::new(15).unwrap();
    let i = Dag1Index::new(7).unwrap();
    let m = Dag1Modify::new(7).unwrap();
    assert_eq!(
        Instruction48::DmStore {
            src: r,
            index: i,
            modifier: m,
            access: DmAccess::PreModify
        }
        .encode()
        .unwrap()
        .bits(),
        0x4ffe87800000
    );
    assert_eq!(
        Instruction48::DmOffsetStore {
            src: r,
            index: i,
            offset: i32::MIN
        }
        .encode()
        .unwrap()
        .bits(),
        0xaf0f80000000
    );
    assert_eq!(
        Instruction48::DmOffsetLoad {
            dst: r,
            index: i,
            offset: i32::MAX
        }
        .encode()
        .unwrap()
        .bits(),
        0xae0f7fffffff
    );
    assert_eq!(
        Instruction48::ModifyWords {
            index: i,
            words: i32::MIN
        }
        .encode()
        .unwrap()
        .bits(),
        0x158780000000
    );
    // The plain form: opcode 0x16, W = 0 (stock noisegate 0x057 `I4=MODIFY(I4,0x2c)`).
    assert_eq!(
        Instruction48::ModifyBytes {
            index: i,
            bytes: i32::MIN
        }
        .encode()
        .unwrap()
        .bits(),
        0x160780000000
    );
}

#[test]
fn branch_address_and_displacement_limits_do_not_truncate() {
    for (delta, expected) in [(-0x800000, 0x073e00800000), (0x7fffff, 0x073e007fffff)] {
        assert_eq!(
            Instruction48::Branch {
                kind: BranchKind::Jump,
                condition: Condition::Always,
                target: BranchTarget::Relative(PcRelative24::new(delta).unwrap())
            }
            .encode()
            .unwrap()
            .bits(),
            expected
        );
    }
    assert!(PcRelative24::new(-0x800001).is_err());
    assert!(PcRelative24::new(0x800000).is_err());
    assert!(PcRelative24::new(i64::MIN).is_err());
    assert!(PmWordAddress24::new(0x1000000).is_err());
    assert_eq!(
        Instruction48::Branch {
            kind: BranchKind::Jump,
            condition: Condition::Always,
            target: BranchTarget::Absolute(PmWordAddress24::new(0xffffff).unwrap())
        }
        .encode()
        .unwrap()
        .bits(),
        0x063e00ffffff
    );
}

#[test]
fn relative_labels_use_branch_start_in_pc_words() {
    let mut p = IsaProgram::new(PmWordAddress24::new(0x654321).unwrap());
    let start = Label::new("start");
    let end = Label::new("end");
    p.define(start.clone()).unwrap();
    p.branch_to(
        BranchKind::Jump,
        Condition::Equal,
        end.clone(),
        BranchRelocation::PcRel24,
    )
    .unwrap();
    p.push(Instruction48::Nop).unwrap();
    p.branch_to(
        BranchKind::Jump,
        Condition::NotEqual,
        start,
        BranchRelocation::PcRel24,
    )
    .unwrap();
    p.define(end).unwrap();
    p.push(Instruction48::Rts).unwrap();
    let code = p.finish().unwrap();
    assert_eq!(code.words()[0].bits(), 0x070000000003);
    assert_eq!(code.words()[2].bits(), 0x072000fffffe);
}

#[test]
fn absolute_label_call_retains_branch_class_and_condition() {
    let mut p = IsaProgram::new(PmWordAddress24::new(0x123455).unwrap());
    let callee = Label::new("callee");
    p.branch_to(
        BranchKind::Call,
        Condition::Always,
        callee.clone(),
        BranchRelocation::AbsPm24,
    )
    .unwrap();
    p.define(callee).unwrap();
    p.push(Instruction48::Rts).unwrap();
    assert_eq!(p.finish().unwrap().words()[0].bits(), 0x06be00123456);
}

#[test]
fn duplicate_undefined_and_noninstruction_labels_fail() {
    let mut p = IsaProgram::new(PmWordAddress24::new(0).unwrap());
    let label = Label::new("same");
    p.define(label.clone()).unwrap();
    assert_eq!(
        p.define(label.clone()),
        Err(EmitError::DuplicateLabel(label.clone()))
    );
    p.push(Instruction48::Nop).unwrap();
    p.branch_to(
        BranchKind::Jump,
        Condition::Always,
        Label::new("missing"),
        BranchRelocation::PcRel24,
    )
    .unwrap();
    assert_eq!(
        p.finish(),
        Err(EmitError::UndefinedLabel(Label::new("missing")))
    );
    let mut p = IsaProgram::new(PmWordAddress24::new(0).unwrap());
    p.branch_to(
        BranchKind::Jump,
        Condition::Always,
        label.clone(),
        BranchRelocation::PcRel24,
    )
    .unwrap();
    p.define(label.clone()).unwrap();
    assert_eq!(
        p.finish(),
        Err(EmitError::LabelBoundary { label, offset: 1 })
    );
}

#[test]
fn program_bounds_reject_append_and_gain_overflow() {
    let mut p = IsaProgram::new(PmWordAddress24::new(0xffffff).unwrap());
    p.push(Instruction48::Rts).unwrap();
    assert!(matches!(
        p.push(Instruction48::Nop),
        Err(EmitError::ProgramBounds { .. })
    ));
    assert!(matches!(
        compile_gain(PmWordAddress24::new(0xfffff8).unwrap()),
        Err(EmitError::ProgramBounds { .. })
    ));
}

#[test]
fn unsupported_features_and_flags_produce_no_partial_instruction() {
    let mut p = IsaProgram::new(PmWordAddress24::new(0).unwrap());
    for feature in [
        UnsupportedFeature::ProgramMemory,
        UnsupportedFeature::Visa,
        UnsupportedFeature::Simd,
        UnsupportedFeature::ParallelCompute,
        UnsupportedFeature::CrossIndexModify,
        UnsupportedFeature::IndirectCall,
        UnsupportedFeature::HardwareLoop,
        UnsupportedFeature::DelayedBranch,
        UnsupportedFeature::SystemRegister,
        UnsupportedFeature::ResidentBoot,
        UnsupportedFeature::Dma,
        UnsupportedFeature::ContextAbi,
    ] {
        assert_eq!(
            p.push(Instruction48::Unsupported(feature)),
            Err(EmitError::Encode(EncodeError::Unsupported(feature)))
        );
    }
    assert!(matches!(
        Mode1Mask::new(1 << 31),
        Err(EncodeError::Mode1Mask(_))
    ));
    assert_eq!(
        Instruction48::Mode1Set(Mode1Mask::new(MODE1_PEYEN).unwrap()).encode(),
        Err(EncodeError::Unsupported(UnsupportedFeature::Simd))
    );
    p.push(Instruction48::Rts).unwrap();
    assert_eq!(p.finish().unwrap().word_count(), 1);
}

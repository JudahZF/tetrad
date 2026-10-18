use tetrad_sharc::address::{DmWordAddress32, PcRelative24, PmWordAddress24};
use tetrad_sharc::emit::{BranchRelocation, IsaProgram, Label, OwnedCode};
use tetrad_sharc::isa48::{
    BitOperation, BranchKind, BranchTarget, Condition, Instruction48, MODE1_PEYEN, Mmask,
    Mode1Mask, STKYX_PCEM, StackEmptyMask, StackPops, SystemBitMask,
};
use tetrad_sharc::register::{DataReg, UniversalReg};
use tetrad_sharc::simulator::{
    DmMemory, PC_STACK_CAPACITY, ScalarState, SimulationError, UnmodeledSemantics, run_linked,
    run_scalar,
};

#[test]
fn invented_nested_calls_match_all_ten_raw_words_and_return_pc_plus_one() {
    let mut program = IsaProgram::new(PmWordAddress24::new(0x200).unwrap());
    let inner = Label::new("inner");
    let outer = Label::new("outer");
    let done = Label::new("done");
    program
        .branch_to(
            BranchKind::Call,
            Condition::Always,
            outer.clone(),
            BranchRelocation::AbsPm24,
        )
        .unwrap();
    program
        .push(Instruction48::Immediate {
            dst: UniversalReg::Data(DataReg::new(0).unwrap()),
            value: 10,
        })
        .unwrap();
    program
        .branch_to(
            BranchKind::Jump,
            Condition::Always,
            done.clone(),
            BranchRelocation::AbsPm24,
        )
        .unwrap();
    program.push(Instruction48::Nop).unwrap();
    program.define(outer).unwrap();
    program
        .branch_to(
            BranchKind::Call,
            Condition::Always,
            inner.clone(),
            BranchRelocation::AbsPm24,
        )
        .unwrap();
    program
        .push(Instruction48::Immediate {
            dst: UniversalReg::Data(DataReg::new(1).unwrap()),
            value: 11,
        })
        .unwrap();
    program.push(Instruction48::Rts).unwrap();
    program.define(inner).unwrap();
    program
        .push(Instruction48::Immediate {
            dst: UniversalReg::Data(DataReg::new(2).unwrap()),
            value: 12,
        })
        .unwrap();
    program.push(Instruction48::Rts).unwrap();
    program.define(done.clone()).unwrap();
    program.push(Instruction48::Nop).unwrap();
    let code = program.finish().unwrap();
    let expected = [
        0x06be00000204,
        0x0f000000000a,
        0x063e00000209,
        0,
        0x06be00000207,
        0x0f010000000b,
        0x0a3e00000000,
        0x0f020000000c,
        0x0a3e00000000,
        0,
    ];
    assert_eq!(
        code.words().iter().map(|w| w.bits()).collect::<Vec<_>>(),
        expected
    );
    let mut state = ScalarState::default();
    let mut memory = DmMemory::new(DmWordAddress32::new(0), vec![]).unwrap();
    let outcome = run_linked(&code, &mut state, &mut memory, code.symbols()[&done], 8).unwrap();
    assert_eq!(outcome.steps, 8); // Stops before the terminal NOP.
    assert_eq!(outcome.stores, 0);
    assert_eq!(state.data[..3], [10, 11, 12]);
    assert_eq!(state.data[3..], [0; 13]);
    assert_eq!(
        run_linked(&code, &mut state, &mut memory, code.symbols()[&done], 7),
        Err(SimulationError::StepBudget(7))
    );
}

// Each function makes one direct call then returns. Thirty calls fit exactly;
// a thirty-first call must fail rather than overwrite a return address.
fn call_chain(depth: usize) -> (OwnedCode, PmWordAddress24) {
    let mut program = IsaProgram::new(PmWordAddress24::new(0x400).unwrap());
    let functions: Vec<_> = (0..depth).map(|n| Label::new(format!("f{n}"))).collect();
    let done = Label::new("done");
    program
        .branch_to(
            BranchKind::Call,
            Condition::Always,
            functions[0].clone(),
            BranchRelocation::AbsPm24,
        )
        .unwrap();
    program
        .branch_to(
            BranchKind::Jump,
            Condition::Always,
            done.clone(),
            BranchRelocation::AbsPm24,
        )
        .unwrap();
    for (n, function) in functions.iter().enumerate() {
        program.define(function.clone()).unwrap();
        if let Some(next) = functions.get(n + 1) {
            program
                .branch_to(
                    BranchKind::Call,
                    Condition::Always,
                    next.clone(),
                    BranchRelocation::AbsPm24,
                )
                .unwrap();
        }
        program.push(Instruction48::Rts).unwrap();
    }
    program.define(done.clone()).unwrap();
    program.push(Instruction48::Nop).unwrap();
    let code = program.finish().unwrap();
    let stop = code.symbols()[&done];
    (code, stop)
}

#[test]
fn pc_stack_capacity_underflow_and_nonempty_stop_are_explicit() {
    let mut memory = DmMemory::new(DmWordAddress32::new(0), vec![]).unwrap();
    assert_eq!(PC_STACK_CAPACITY, 30);
    let (code, stop) = call_chain(30);
    assert_eq!(
        run_linked(&code, &mut ScalarState::default(), &mut memory, stop, 61)
            .unwrap()
            .steps,
        61
    );
    let (code, stop) = call_chain(31);
    assert_eq!(
        run_linked(&code, &mut ScalarState::default(), &mut memory, stop, 100),
        Err(SimulationError::PcStackOverflow(0x43c))
    );
    let mut program = IsaProgram::new(PmWordAddress24::new(0x200).unwrap());
    program.push(Instruction48::Rts).unwrap();
    program.push(Instruction48::Nop).unwrap();
    let code = program.finish().unwrap();
    assert_eq!(
        run_linked(
            &code,
            &mut ScalarState::default(),
            &mut memory,
            PmWordAddress24::new(0x201).unwrap(),
            2
        ),
        Err(SimulationError::PcStackUnderflow(0x200))
    );
    // Preserve the existing leaf-to-host convention as a separate API.
    assert_eq!(
        run_scalar(&code, &mut ScalarState::default(), &mut memory, 1)
            .unwrap()
            .steps,
        1
    );
    let mut program = IsaProgram::new(PmWordAddress24::new(0x200).unwrap());
    program
        .push(Instruction48::Branch {
            kind: BranchKind::Call,
            condition: Condition::Always,
            target: BranchTarget::Absolute(PmWordAddress24::new(0x201).unwrap()),
        })
        .unwrap();
    program.push(Instruction48::Nop).unwrap();
    let code = program.finish().unwrap();
    assert_eq!(
        run_linked(
            &code,
            &mut ScalarState::default(),
            &mut memory,
            PmWordAddress24::new(0x201).unwrap(),
            1
        ),
        Err(SimulationError::PcStackNotEmpty(1))
    );
}

#[test]
fn calls_and_jumps_reject_targets_outside_linked_code() {
    let mut memory = DmMemory::new(DmWordAddress32::new(0), vec![]).unwrap();
    for kind in [BranchKind::Call, BranchKind::Jump] {
        for (target, expected) in [
            (
                BranchTarget::Absolute(PmWordAddress24::new(0x1ff).unwrap()),
                0x1ff,
            ),
            (
                BranchTarget::Absolute(PmWordAddress24::new(0x202).unwrap()),
                0x202,
            ),
            (
                BranchTarget::Absolute(PmWordAddress24::new(0xffffff).unwrap()),
                0xffffff,
            ),
            (
                BranchTarget::Relative(PcRelative24::new(-1).unwrap()),
                0x1ff,
            ),
            (BranchTarget::Relative(PcRelative24::new(2).unwrap()), 0x202),
        ] {
            let mut program = IsaProgram::new(PmWordAddress24::new(0x200).unwrap());
            program
                .push(Instruction48::Branch {
                    kind,
                    condition: Condition::Always,
                    target,
                })
                .unwrap();
            program.push(Instruction48::Nop).unwrap();
            let code = program.finish().unwrap();
            assert_eq!(
                run_linked(
                    &code,
                    &mut ScalarState::default(),
                    &mut memory,
                    PmWordAddress24::new(0x201).unwrap(),
                    1
                ),
                Err(SimulationError::CodeTarget(expected))
            );
            assert_eq!(
                run_linked(
                    &code,
                    &mut ScalarState::default(),
                    &mut memory,
                    PmWordAddress24::new(0x202).unwrap(),
                    1
                ),
                Err(SimulationError::CodeTarget(0x202))
            );
        }
    }
}

#[test]
fn conditional_call_does_not_push_when_not_taken_and_nop_consumes_budget() {
    for zero in [false, true] {
        let mut program = IsaProgram::new(PmWordAddress24::new(0x200).unwrap());
        let callee = Label::new("callee");
        let done = Label::new("done");
        program.push(Instruction48::Nop).unwrap();
        program
            .branch_to(
                BranchKind::Call,
                Condition::Equal,
                callee.clone(),
                BranchRelocation::PcRel24,
            )
            .unwrap();
        program
            .branch_to(
                BranchKind::Jump,
                Condition::Always,
                done.clone(),
                BranchRelocation::AbsPm24,
            )
            .unwrap();
        program.define(callee).unwrap();
        program
            .push(Instruction48::Immediate {
                dst: UniversalReg::Data(DataReg::new(7).unwrap()),
                value: 7,
            })
            .unwrap();
        program.push(Instruction48::Rts).unwrap();
        program.define(done.clone()).unwrap();
        program.push(Instruction48::Nop).unwrap();
        let code = program.finish().unwrap();
        let mut state = ScalarState {
            alu_zero: zero,
            ..Default::default()
        };
        let mut memory = DmMemory::new(DmWordAddress32::new(0), vec![]).unwrap();
        let budget = if zero { 5 } else { 3 };
        assert_eq!(
            run_linked(
                &code,
                &mut state,
                &mut memory,
                code.symbols()[&done],
                budget
            )
            .unwrap()
            .steps,
            budget
        );
        assert_eq!(state.data[7], if zero { 7 } else { 0 });
    }
}

#[test]
fn immediate_direct_dm_uses_full_address_and_preserves_canaries_and_dag() {
    for base in [0, 0x12345678, u32::MAX - 3] {
        let mut program = IsaProgram::new(PmWordAddress24::new(0x200).unwrap());
        program
            .push(Instruction48::DmDirectLoad {
                dst: DataReg::new(15).unwrap(),
                address: DmWordAddress32::new(base + 1),
            })
            .unwrap();
        program
            .push(Instruction48::DmDirectStore {
                src: DataReg::new(15).unwrap(),
                address: DmWordAddress32::new(base + 2),
            })
            .unwrap();
        program.push(Instruction48::Rts).unwrap();
        let code = program.finish().unwrap();
        let mut state = ScalarState {
            indices: [0xf000; 8],
            modifiers: [-17; 8],
            bases: [0x7000; 8],
            lengths: [9; 8],
            ..Default::default()
        };
        let before = state.clone();
        let mut memory = DmMemory::new(
            DmWordAddress32::new(base),
            vec![0xdead, 0x1234abcd, 0x7777, 0xbeef],
        )
        .unwrap();
        let outcome = run_scalar(&code, &mut state, &mut memory, 3).unwrap();
        assert_eq!(outcome.stores, 1);
        assert_eq!(memory.words(), [0xdead, 0x1234abcd, 0x1234abcd, 0xbeef]);
        assert_eq!(memory.writes(), [DmWordAddress32::new(base + 2)]);
        assert_eq!(state.data[15], 0x1234abcd);
        assert_eq!(state.data[..15], before.data[..15]);
        assert_eq!(state.indices, before.indices);
        assert_eq!(state.modifiers, before.modifiers);
        assert_eq!(state.bases, before.bases);
        assert_eq!(state.lengths, before.lengths);
        assert_eq!(state.mode1, before.mode1);
        assert_eq!(state.alu_zero, before.alu_zero);
    }
    // A normal-word region may include address32 MAX; direct access has no
    // one-past DAG update and must not apply the gain leaf's stricter rule.
    let mut program = IsaProgram::new(PmWordAddress24::new(0).unwrap());
    program
        .push(Instruction48::DmDirectLoad {
            dst: DataReg::new(0).unwrap(),
            address: DmWordAddress32::new(u32::MAX),
        })
        .unwrap();
    program
        .push(Instruction48::DmDirectStore {
            src: DataReg::new(0).unwrap(),
            address: DmWordAddress32::new(u32::MAX),
        })
        .unwrap();
    program.push(Instruction48::Rts).unwrap();
    let mut memory = DmMemory::new(DmWordAddress32::new(u32::MAX), vec![0x2345]).unwrap();
    run_scalar(
        &program.finish().unwrap(),
        &mut ScalarState::default(),
        &mut memory,
        3,
    )
    .unwrap();
    assert_eq!(memory.words(), [0x2345]);
}

#[test]
fn direct_dm_has_only_caller_region_bounds_and_no_mmio_side_effects() {
    for address in [0xff, 0x103, u32::MAX] {
        for store in [false, true] {
            let mut program = IsaProgram::new(PmWordAddress24::new(0x200).unwrap());
            let reg = DataReg::new(7).unwrap();
            let address = DmWordAddress32::new(address);
            program
                .push(if store {
                    Instruction48::DmDirectStore { src: reg, address }
                } else {
                    Instruction48::DmDirectLoad { dst: reg, address }
                })
                .unwrap();
            program.push(Instruction48::Rts).unwrap();
            let mut state = ScalarState::default();
            state.data[7] = 0x99;
            let before = state.clone();
            let mut memory =
                DmMemory::new(DmWordAddress32::new(0x100), vec![0x11, 0x22, 0x33]).unwrap();
            let memory_before = memory.clone();
            assert_eq!(
                run_scalar(&program.finish().unwrap(), &mut state, &mut memory, 2),
                Err(SimulationError::MemoryBounds {
                    address: address.get(),
                    words: 1
                })
            );
            assert_eq!(state, before);
            assert_eq!(memory, memory_before);
        }
    }
}

#[test]
fn rti_stack_cache_and_system_register_execution_have_explicit_errors() {
    for (instruction, feature) in [
        (Instruction48::Rti, UnmodeledSemantics::InterruptReturn),
        (
            Instruction48::PopStacks {
                stacks: StackPops::Pc,
                flush_conflict_cache: false,
            },
            UnmodeledSemantics::StackMaintenance,
        ),
        (
            Instruction48::PopStacks {
                stacks: StackPops::Status,
                flush_conflict_cache: false,
            },
            UnmodeledSemantics::StackMaintenance,
        ),
        (
            Instruction48::PopStacks {
                stacks: StackPops::Loop,
                flush_conflict_cache: false,
            },
            UnmodeledSemantics::StackMaintenance,
        ),
        (
            Instruction48::FlushConflictCache,
            UnmodeledSemantics::ConflictCache,
        ),
        (
            Instruction48::Mode1Clear(Mode1Mask::new(MODE1_PEYEN).unwrap()),
            UnmodeledSemantics::SystemRegisters,
        ),
        (
            Instruction48::MmaskImmediate(Mmask::new(MODE1_PEYEN).unwrap()),
            UnmodeledSemantics::SystemRegisters,
        ),
        (
            Instruction48::SystemBit {
                operation: BitOperation::Test,
                operand: SystemBitMask::StackEmpty(StackEmptyMask::new(STKYX_PCEM).unwrap()),
            },
            UnmodeledSemantics::SystemRegisters,
        ),
        (
            Instruction48::Branch {
                kind: BranchKind::Jump,
                condition: Condition::TestFlag,
                target: BranchTarget::Relative(PcRelative24::new(1).unwrap()),
            },
            UnmodeledSemantics::TestFlag,
        ),
        (
            Instruction48::Branch {
                kind: BranchKind::Jump,
                condition: Condition::NotTestFlag,
                target: BranchTarget::Relative(PcRelative24::new(1).unwrap()),
            },
            UnmodeledSemantics::TestFlag,
        ),
    ] {
        let mut program = IsaProgram::new(PmWordAddress24::new(0x200).unwrap());
        program.push(instruction).unwrap();
        program.push(Instruction48::Rts).unwrap();
        let mut memory = DmMemory::new(DmWordAddress32::new(0), vec![0x1111]).unwrap();
        let before_memory = memory.clone();
        let mut state = ScalarState::default();
        let before_state = state.clone();
        assert_eq!(
            run_scalar(&program.finish().unwrap(), &mut state, &mut memory, 2),
            Err(SimulationError::Unmodeled { pc: 0x200, feature })
        );
        assert_eq!(state, before_state);
        assert_eq!(memory, before_memory);
    }
}

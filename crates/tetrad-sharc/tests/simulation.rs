use tetrad_sharc::address::{AddressError, DmWordAddress32, PmWordAddress24};
use tetrad_sharc::emit::{BranchRelocation, IsaProgram, Label, OwnedCode};
use tetrad_sharc::gain::{GAIN_ABI, compile_gain};
use tetrad_sharc::isa48::{
    BranchKind, BranchTarget, Condition, DmAccess, Instruction48, MODE1_CBUFEN, MODE1_PEYEN,
    MODE1_RND32,
};
use tetrad_sharc::register::{Dag1Index, Dag1Modify, DataReg, FloatReg, UniversalReg};
use tetrad_sharc::simulator::{
    DmMemory, GainInvocation, SampleCount, ScalarState, SimulationError, run_scalar,
    run_scalar_normal_floats, run_scalar_prm, simulate_gain,
};

#[test]
fn gain_math_zero_one_and_32_separate_and_in_place_with_preservation() {
    let code = compile_gain(PmWordAddress24::new(0x12000).unwrap()).unwrap();
    for count in [0, 1, 32] {
        for gain in [0.0, 1.0, 0.5, -0.75] {
            for in_place in [false, true] {
                let base = DmWordAddress32::new(0x4123);
                let input = base.checked_add(3).unwrap();
                let output_offset = if in_place { 3 } else { 51 };
                let output = base.checked_add(output_offset).unwrap();
                let mut data: Vec<u32> = (0..100).map(|n| 0xa501_0000 + n * 0x103).collect();
                for n in 0..32 {
                    data[n + 3] = ((n as f32 - 13.75) * 0.375).to_bits();
                }
                let mut expected = data.clone();
                for n in 0..count as usize {
                    expected[n + output_offset as usize] =
                        (f32::from_bits(data[n + 3]) * gain).to_bits();
                }
                let mut memory = DmMemory::new(base, data).unwrap();
                // Different nonzero values in all unrelated registers expose index
                // and preservation mistakes, including accidental use of M4.
                let mut state = ScalarState {
                    data: std::array::from_fn(|n| 0x8100_0101 + n as u32 * 0x10203),
                    indices: std::array::from_fn(|n| 0x7100_0020 + n as u32 * 0x103),
                    modifiers: std::array::from_fn(|n| -31 + n as i32 * 7),
                    bases: std::array::from_fn(|n| 0x2100 + n as u32 * 0x313),
                    lengths: [0, 0, 12, 13, 14, 15, 16, 17],
                    mode1: MODE1_RND32 | MODE1_CBUFEN,
                    alu_zero: true,
                };
                state.indices[0] = input.get();
                state.indices[1] = output.get();
                state.data[4] = gain.to_bits();
                state.data[8] = count;
                let before = state.clone();
                let invocation =
                    GainInvocation::new(input, output, SampleCount::new(count).unwrap(), gain)
                        .unwrap();
                let result =
                    simulate_gain(&code, &mut state, &mut memory, invocation, 200).unwrap();
                assert_eq!(result.steps, 4 + 5 * count as usize);
                assert_eq!(result.stores, count as usize);
                assert_eq!(memory.words(), expected);
                assert_eq!(
                    memory.writes(),
                    (0..count)
                        .map(|n| output.checked_add(n).unwrap())
                        .collect::<Vec<_>>()
                );
                for n in 0..16 {
                    if !GAIN_ABI
                        .clobbered_data
                        .iter()
                        .any(|r| usize::from(r.index()) == n)
                    {
                        assert_eq!(state.data[n], before.data[n], "R{n}");
                    }
                }
                for n in 0..8 {
                    if n > 1 {
                        assert_eq!(state.indices[n], before.indices[n], "I{n}");
                    }
                    if n != 0 {
                        assert_eq!(state.modifiers[n], before.modifiers[n], "M{n}");
                    }
                }
                assert_eq!(state.bases, before.bases);
                assert_eq!(state.lengths, before.lengths);
                assert_eq!(state.mode1, before.mode1);
                assert_eq!(state.indices[0], input.get() + count);
                assert_eq!(state.indices[1], output.get() + count);
                assert_eq!(state.data[2], 0);
                assert_eq!(state.modifiers[0], 1);
                assert!(state.alu_zero);
            }
        }
    }
}

#[test]
fn caller_count_and_gain_are_checked() {
    assert_eq!(SampleCount::new(33), Err(SimulationError::Count(33)));
    assert_eq!(
        SampleCount::new(u32::MAX),
        Err(SimulationError::Count(u32::MAX))
    );
    for gain in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
        assert_eq!(
            GainInvocation::new(
                DmWordAddress32::new(0),
                DmWordAddress32::new(0),
                SampleCount::new(0).unwrap(),
                gain
            ),
            Err(SimulationError::NonFiniteGain)
        );
    }
}

#[test]
fn partial_overlap_both_directions_and_bad_bounds_fail_before_mutation() {
    let base = DmWordAddress32::new(0x100);
    let code = compile_gain(PmWordAddress24::new(0).unwrap()).unwrap();
    for (input, output, count, overlap) in [
        (0x100, 0x101, 4, true),
        (0x101, 0x100, 4, true),
        (0xff, 0x100, 1, false),
        (0x100, 0x110, 1, false),
        (0x10f, 0x100, 2, false),
    ] {
        let mut memory = DmMemory::new(base, vec![0xdeadbeef; 16]).unwrap();
        let mut state = ScalarState::default();
        let old_memory = memory.clone();
        let old_state = state.clone();
        let invocation = GainInvocation::new(
            DmWordAddress32::new(input),
            DmWordAddress32::new(output),
            SampleCount::new(count).unwrap(),
            0.5,
        )
        .unwrap();
        let error = simulate_gain(&code, &mut state, &mut memory, invocation, 200).unwrap_err();
        if overlap {
            assert_eq!(error, SimulationError::PartialOverlap);
        } else {
            assert!(matches!(error, SimulationError::MemoryBounds { .. }));
        }
        assert_eq!(memory, old_memory);
        assert_eq!(state, old_state);
    }
}

#[test]
fn adjacent_buffers_and_empty_one_past_ranges_are_legal() {
    let memory = DmMemory::new(DmWordAddress32::new(0x100), vec![0; 8]).unwrap();
    GainInvocation::new(
        DmWordAddress32::new(0x100),
        DmWordAddress32::new(0x104),
        SampleCount::new(4).unwrap(),
        1.0,
    )
    .unwrap()
    .validate(&memory)
    .unwrap();
    GainInvocation::new(
        DmWordAddress32::new(0x108),
        DmWordAddress32::new(0x108),
        SampleCount::new(0).unwrap(),
        1.0,
    )
    .unwrap()
    .validate(&memory)
    .unwrap();
    assert!(memory.read(DmWordAddress32::new(0x108)).is_err());
}

#[test]
fn dm_memory_and_one_past_index_arithmetic_do_not_wrap() {
    assert_eq!(
        DmWordAddress32::new(u32::MAX).checked_add(1),
        Err(AddressError::DmOverflow)
    );
    assert_eq!(
        DmMemory::new(DmWordAddress32::new(u32::MAX), vec![0; 2]),
        Err(SimulationError::Address(AddressError::DmOverflow))
    );
    let mut memory = DmMemory::new(DmWordAddress32::new(u32::MAX), vec![0xdeadbeef]).unwrap();
    let invocation = GainInvocation::new(
        DmWordAddress32::new(u32::MAX),
        DmWordAddress32::new(u32::MAX),
        SampleCount::new(1).unwrap(),
        1.0,
    )
    .unwrap();
    assert_eq!(
        invocation.validate(&memory),
        Err(SimulationError::Address(AddressError::DmOverflow))
    );
    assert!(memory.write(DmWordAddress32::new(0), 1).is_err());
    assert!(memory.writes().is_empty());
    assert_eq!(memory.words(), [0xdeadbeef]);
}

#[test]
fn mode_and_circular_length_preconditions_fail_before_mutation() {
    let code = compile_gain(PmWordAddress24::new(0).unwrap()).unwrap();
    let invocation = GainInvocation::new(
        DmWordAddress32::new(0),
        DmWordAddress32::new(4),
        SampleCount::new(1).unwrap(),
        0.5,
    )
    .unwrap();
    for state in [
        ScalarState {
            mode1: MODE1_RND32 | MODE1_PEYEN,
            ..Default::default()
        },
        ScalarState {
            mode1: 0,
            ..Default::default()
        },
        ScalarState {
            lengths: [1, 0, 0, 0, 0, 0, 0, 0],
            ..Default::default()
        },
        ScalarState {
            lengths: [0, 3, 0, 0, 0, 0, 0, 0],
            ..Default::default()
        },
    ] {
        let mut state = state;
        let before = state.clone();
        let mut memory = DmMemory::new(DmWordAddress32::new(0), vec![0; 8]).unwrap();
        assert!(simulate_gain(&code, &mut state, &mut memory, invocation, 20).is_err());
        assert_eq!(state, before);
        assert!(memory.writes().is_empty());
    }
}

#[test]
fn missing_rts_and_step_budget_are_distinct_errors() {
    let mut p = IsaProgram::new(PmWordAddress24::new(0x123).unwrap());
    p.push(Instruction48::Nop).unwrap();
    let code = p.finish().unwrap();
    let mut memory = DmMemory::new(DmWordAddress32::new(0), vec![]).unwrap();
    assert_eq!(
        run_scalar(&code, &mut ScalarState::default(), &mut memory, 2),
        Err(SimulationError::MissingRts(0x124))
    );
    let mut p = IsaProgram::new(PmWordAddress24::new(0x123).unwrap());
    let again = Label::new("again");
    p.define(again.clone()).unwrap();
    p.branch_to(
        BranchKind::Jump,
        Condition::Always,
        again,
        BranchRelocation::PcRel24,
    )
    .unwrap();
    let code = p.finish().unwrap();
    assert_eq!(
        run_scalar(&code, &mut ScalarState::default(), &mut memory, 7),
        Err(SimulationError::StepBudget(7))
    );
    assert_eq!(
        run_scalar(&code, &mut ScalarState::default(), &mut memory, 0),
        Err(SimulationError::StepBudget(0))
    );
}

#[test]
fn bounded_gain_budget_and_instances_are_independent() {
    let code = compile_gain(PmWordAddress24::new(0).unwrap()).unwrap();
    let invocation = GainInvocation::new(
        DmWordAddress32::new(0),
        DmWordAddress32::new(1),
        SampleCount::new(1).unwrap(),
        0.5,
    )
    .unwrap();
    let mut memory = DmMemory::new(DmWordAddress32::new(0), vec![6.0f32.to_bits(), 0]).unwrap();
    assert_eq!(
        simulate_gain(
            &code,
            &mut ScalarState::default(),
            &mut memory,
            invocation,
            8
        ),
        Err(SimulationError::StepBudget(8))
    );
    for value in [10.0f32, -8.0] {
        let mut state = ScalarState::default();
        let mut memory = DmMemory::new(DmWordAddress32::new(0), vec![value.to_bits(), 0]).unwrap();
        let result = simulate_gain(&code, &mut state, &mut memory, invocation, 9).unwrap();
        assert_eq!(result.steps, 9);
        assert_eq!(memory.words()[1], (value * 0.5).to_bits());
        assert_eq!(state.data[4], 0.5f32.to_bits());
    }
}

#[test]
fn simulator_supports_call_but_checks_the_return_target() {
    let mut p = IsaProgram::new(PmWordAddress24::new(0).unwrap());
    p.push(Instruction48::Branch {
        kind: BranchKind::Call,
        condition: Condition::Always,
        target: BranchTarget::Absolute(PmWordAddress24::new(0).unwrap()),
    })
    .unwrap();
    let code = p.finish().unwrap();
    let mut memory = DmMemory::new(DmWordAddress32::new(0), vec![]).unwrap();
    assert!(matches!(
        run_scalar(&code, &mut ScalarState::default(), &mut memory, 10),
        Err(SimulationError::CodeTarget(1))
    ));
}

#[test]
fn raw_decode_pre_modify_uses_m5_and_does_not_update_i2() {
    let mut p = IsaProgram::new(PmWordAddress24::new(0x600).unwrap());
    p.push(Instruction48::DmLoad {
        dst: DataReg::new(7).unwrap(),
        index: Dag1Index::new(2).unwrap(),
        modifier: Dag1Modify::new(5).unwrap(),
        access: DmAccess::PreModify,
    })
    .unwrap();
    p.push(Instruction48::Rts).unwrap();
    let code = p.finish().unwrap();
    let mut memory = DmMemory::new(DmWordAddress32::new(0x100), vec![11, 22, 33]).unwrap();
    let mut state = ScalarState::default();
    state.indices[2] = 0x102;
    state.modifiers[5] = -1;
    state.modifiers[4] = -2;
    run_scalar(&code, &mut state, &mut memory, 2).unwrap();
    assert_eq!(state.data[7], 22);
    assert_eq!(state.indices[2], 0x102);
    assert!(memory.writes().is_empty());
}

#[test]
fn raw_decode_float_alu_integer_flags_and_absolute_branch() {
    let mut p = IsaProgram::new(PmWordAddress24::new(0x600).unwrap());
    let f: [FloatReg; 16] = std::array::from_fn(|n| FloatReg::new(n as u8).unwrap());
    p.push(Instruction48::FloatAdd {
        dst: f[6],
        lhs: f[2],
        rhs: f[11],
    })
    .unwrap();
    p.push(Instruction48::FloatSubtract {
        dst: f[7],
        lhs: f[13],
        rhs: f[4],
    })
    .unwrap();
    p.push(Instruction48::IntegerPass {
        dst: DataReg::new(8).unwrap(),
        src: DataReg::new(9).unwrap(),
    })
    .unwrap();
    let done = Label::new("done");
    p.branch_to(
        BranchKind::Jump,
        Condition::Equal,
        done.clone(),
        BranchRelocation::AbsPm24,
    )
    .unwrap();
    p.push(Instruction48::Immediate {
        dst: UniversalReg::Data(DataReg::new(0).unwrap()),
        value: 99,
    })
    .unwrap();
    p.define(done).unwrap();
    p.push(Instruction48::Rts).unwrap();
    let mut state = ScalarState::default();
    state.data[2] = 1.5f32.to_bits();
    state.data[11] = 2.25f32.to_bits();
    state.data[13] = 3.0f32.to_bits();
    state.data[4] = 0.75f32.to_bits();
    let mut memory = DmMemory::new(DmWordAddress32::new(0), vec![]).unwrap();
    let outcome = run_scalar(&p.finish().unwrap(), &mut state, &mut memory, 6).unwrap();
    assert_eq!(outcome.steps, 5);
    assert_eq!(state.data[6], 3.75f32.to_bits());
    assert_eq!(state.data[7], 2.25f32.to_bits());
    assert_eq!(state.data[0], 0);
}

/// ADAA select shape: `m = max(|x - p|, eps)`, `comp(m, eps)`, `IF EQ` midpoint.
#[test]
fn compare_branch_selects_adaa_path_identically_under_both_float_policies() {
    let f: [FloatReg; 16] = std::array::from_fn(|n| FloatReg::new(n as u8).unwrap());
    let r: [DataReg; 16] = std::array::from_fn(|n| DataReg::new(n as u8).unwrap());
    let mut p = IsaProgram::new(PmWordAddress24::new(0x700).unwrap());
    let midpoint = Label::new("midpoint");
    for instruction in [
        Instruction48::FloatSubtract {
            dst: f[5],
            lhs: f[1],
            rhs: f[0],
        },
        Instruction48::FloatAbs {
            dst: f[2],
            src: f[5],
        },
        Instruction48::FloatMax {
            dst: f[2],
            lhs: f[2],
            rhs: f[3],
        },
        Instruction48::FloatCompare {
            lhs: f[2],
            rhs: f[3],
        },
    ] {
        p.push(instruction).unwrap();
    }
    p.branch_to(
        BranchKind::Jump,
        Condition::Equal,
        midpoint.clone(),
        BranchRelocation::AbsPm24,
    )
    .unwrap();
    for instruction in [
        Instruction48::FloatCopySign {
            dst: f[6],
            magnitude: f[7],
            sign: f[5],
        },
        Instruction48::IntegerSubtract {
            dst: r[8],
            lhs: r[9],
            rhs: r[2],
        },
        Instruction48::Rts,
    ] {
        p.push(instruction).unwrap();
    }
    p.define(midpoint).unwrap();
    p.push(Instruction48::FloatClip {
        dst: f[6],
        src: f[1],
        bound: f[10],
    })
    .unwrap();
    p.push(Instruction48::Rts).unwrap();
    let code = p.finish().unwrap();

    const EPS: u32 = 0x38d1_b717;
    const MAGIC: u32 = 0x7ef3_11c7;
    // (x, p, steps, m, F6, R8 seed): R8 keeps its sentinel on the midpoint path.
    for (x, previous, steps, m, f6, seed) in [
        (
            0.5f32,
            0.25f32,
            8,
            0.25f32.to_bits(),
            1.0f32,
            MAGIC - 0x3e80_0000,
        ),
        (0.25, 0.5, 8, 0.25f32.to_bits(), -1.0, MAGIC - 0x3e80_0000),
        (3.0, 3.0, 7, EPS, 2.0, 0xdead_beef),
        (1.0, 1.0 - f32::EPSILON / 2.0, 7, EPS, 1.0, 0xdead_beef),
    ] {
        let mut start = ScalarState::default();
        start.data[0] = previous.to_bits();
        start.data[1] = x.to_bits();
        start.data[3] = EPS;
        start.data[7] = 1.0f32.to_bits();
        start.data[8] = 0xdead_beef;
        start.data[9] = MAGIC;
        start.data[10] = 2.0f32.to_bits();
        let mut runs = Vec::new();
        for run in [run_scalar, run_scalar_normal_floats] {
            let mut state = start.clone();
            let mut memory = DmMemory::new(DmWordAddress32::new(0), vec![]).unwrap();
            let outcome = run(&code, &mut state, &mut memory, 8).unwrap();
            assert_eq!(outcome.steps, steps, "{x} {previous}");
            assert_eq!(
                (state.data[0], state.data[2], state.data[6], state.data[8]),
                (previous.to_bits(), m, f6.to_bits(), seed),
                "{x} {previous}"
            );
            assert_eq!(state.data[5], (x - previous).to_bits());
            assert!(!state.alu_zero);
            runs.push(state);
        }
        assert_eq!(runs[0], runs[1]);
    }
}

/// The opt-in policy rejects at the failing compute, before its register or AZ
/// write; `run_scalar` keeps host f32 results for the same inputs.
#[test]
fn normal_float_policy_rejects_operands_and_results_while_default_stays_host_f32() {
    let f: [FloatReg; 16] = std::array::from_fn(|n| FloatReg::new(n as u8).unwrap());
    let r: [DataReg; 16] = std::array::from_fn(|n| DataReg::new(n as u8).unwrap());
    let mut p = IsaProgram::new(PmWordAddress24::new(0x40).unwrap());
    for instruction in [
        Instruction48::FloatMultiply {
            dst: f[2],
            lhs: f[0],
            rhs: f[1],
        },
        Instruction48::FloatAdd {
            dst: f[3],
            lhs: f[2],
            rhs: f[1],
        },
        Instruction48::FloatAbs {
            dst: f[4],
            src: f[3],
        },
        // Integer sources are raw bits: a NaN pattern in R5 is not a float.
        Instruction48::IntegerSubtract {
            dst: r[6],
            lhs: r[5],
            rhs: r[1],
        },
        Instruction48::Rts,
    ] {
        p.push(instruction).unwrap();
    }
    let code = p.finish().unwrap();
    let tiny = f32::from_bits(0x0008_0000);
    let large = f32::from_bits(0x7f40_0000);
    // (F0, F1, rejecting PC): operand, then product, then sum.
    for (f0, f1, pc) in [
        (tiny, 1.0, 0x40),
        (f32::NAN, 1.0, 0x40),
        (1.0, f32::INFINITY, 0x40),
        (2.0f32.powi(-100), 2.0f32.powi(-30), 0x40),
        (1.0, large, 0x41),
        (1.0, 1.0, 0),
    ] {
        let mut start = ScalarState::default();
        start.data[0] = f0.to_bits();
        start.data[1] = f1.to_bits();
        start.data[2] = 0xdead_beef;
        start.data[3] = 0xdead_beef;
        start.data[5] = 0x7fc0_0000;
        start.alu_zero = true;
        let mut memory = DmMemory::new(DmWordAddress32::new(0), vec![]).unwrap();

        let mut host = start.clone();
        run_scalar(&code, &mut host, &mut memory, 5).unwrap();
        let sum = f0 * f1 + f1;
        assert_eq!(host.data[2], (f0 * f1).to_bits());
        assert_eq!(host.data[3], sum.to_bits());
        let abs = if sum.is_nan() {
            u32::MAX
        } else {
            sum.abs().to_bits()
        };
        assert_eq!(host.data[4], abs);
        assert_eq!(host.data[6], 0x7fc0_0000 - f1.to_bits());

        let mut normal = start.clone();
        let result = run_scalar_normal_floats(&code, &mut normal, &mut memory, 5);
        if pc == 0 {
            result.unwrap();
            assert_eq!(normal, host);
            continue;
        }
        assert_eq!(result, Err(SimulationError::NonNormalFloat { pc }));
        let failed = if pc == 0x40 { 2 } else { 3 };
        assert_eq!(normal.data[failed], 0xdead_beef, "{f0} {f1}");
        assert!(normal.alu_zero);
    }
}

const F_SIGN: u32 = 0x8000_0000;

/// `instructions`, then RTS, linked at 0x80.
fn leaf(instructions: impl IntoIterator<Item = Instruction48>) -> OwnedCode {
    let mut p = IsaProgram::new(PmWordAddress24::new(0x80).unwrap());
    for instruction in instructions.into_iter().chain([Instruction48::Rts]) {
        p.push(instruction).unwrap();
    }
    p.finish().unwrap()
}

/// Runs `code` from `start` under the PRM rules with one DM word at 0x10.
/// Returns the registers, their extension fields and the DM word.
fn run_prm(code: &OwnedCode, start: &ScalarState) -> (ScalarState, [u8; 16], u32) {
    let mut state = start.clone();
    let mut extension = [0; 16];
    let mut memory = DmMemory::new(DmWordAddress32::new(0x10), vec![0]).unwrap();
    run_scalar_prm(code, &mut state, &mut extension, &mut memory, 64).unwrap();
    (state, extension, memory.words()[0])
}

fn run_host(code: &OwnedCode, start: &ScalarState) -> ScalarState {
    let mut state = start.clone();
    let mut memory = DmMemory::new(DmWordAddress32::new(0x10), vec![0]).unwrap();
    run_scalar(code, &mut state, &mut memory, 64).unwrap();
    state
}

/// PRM 5.84 and 23.02/.07/.09: denormal operands flush to zero of the same sign
/// (ABS gives +zero) before MUL, ADD, ABS and COMP. Host f32 keeps them.
#[test]
fn prm_denormal_operands_flush_to_signed_zero() {
    let f: [FloatReg; 16] = std::array::from_fn(|n| FloatReg::new(n as u8).unwrap());
    let code = leaf([
        Instruction48::FloatMultiply {
            dst: f[2],
            lhs: f[0],
            rhs: f[1],
        },
        Instruction48::FloatAdd {
            dst: f[3],
            lhs: f[0],
            rhs: f[4],
        },
        Instruction48::FloatAbs {
            dst: f[5],
            src: f[0],
        },
        Instruction48::FloatCompare {
            lhs: f[0],
            rhs: f[6],
        },
    ]);
    let mut start = ScalarState::default();
    start.data[0] = 0x8040_0000; // -2^-127, denormal
    start.data[1] = 0x7180_0000; // 2^100
    start.data[4] = F_SIGN; // -0
    let (prm, extension, _) = run_prm(&code, &start);
    assert_eq!(
        (prm.data[2], prm.data[3], prm.data[5], prm.alu_zero),
        (F_SIGN, F_SIGN, 0, true)
    );
    assert_eq!(extension, [0; 16]);
    let host = run_host(&code, &start);
    assert_eq!(
        (host.data[2], host.data[3], host.data[5], host.alu_zero),
        (0xb200_0000, 0x8040_0000, 0x0040_0000, false)
    );
}

/// PRM 23.02: a post-rounded denormal result returns signed zero. Rounding comes
/// first: one product reaches MIN_NORMAL at 24 bits (RND32) but stays denormal at
/// the 32 bits of the 40-bit default, so it flushes there.
#[test]
fn prm_post_rounded_denormal_results_flush_to_signed_zero() {
    let f: [FloatReg; 16] = std::array::from_fn(|n| FloatReg::new(n as u8).unwrap());
    let code = leaf([
        Instruction48::FloatMultiply {
            dst: f[2],
            lhs: f[0],
            rhs: f[1],
        },
        Instruction48::FloatSubtract {
            dst: f[3],
            lhs: f[4],
            rhs: f[5],
        },
        Instruction48::FloatMultiply {
            dst: f[6],
            lhs: f[7],
            rhs: f[8],
        },
    ]);
    let mut start = ScalarState::default();
    start.data[0] = 0x0d80_0000; // 2^-100
    start.data[1] = 0xb080_0000; // -2^-30: product -2^-130
    start.data[4] = 0x00c0_0000; // 1.5 * 2^-126
    start.data[5] = 0x0080_0000; // 2^-126: difference 2^-127
    // Exact product 2^-126 * (1 - 1956956 * 2^-47): less than 2^-151 below 2^-126.
    start.data[7] = 0x2034_f93b;
    start.data[8] = 0x1fb5_10ac;
    let rnd32 = run_prm(&code, &start).0;
    assert_eq!(
        (rnd32.data[2], rnd32.data[3], rnd32.data[6]),
        (F_SIGN, 0, 0x0080_0000)
    );
    let extended = run_prm(
        &code,
        &ScalarState {
            mode1: 0,
            ..start.clone()
        },
    )
    .0;
    assert_eq!(
        (extended.data[2], extended.data[3], extended.data[6]),
        (F_SIGN, 0, 0)
    );
    let host = run_host(&code, &start);
    assert_eq!(
        (host.data[2], host.data[3], host.data[6]),
        (0x8008_0000, 0x0040_0000, 0x0080_0000)
    );
}

/// PRM 5.84: RND32 rounds to 24 significand bits, as IEEE single does. The reset
/// default (RND32 clear) keeps 32 bits in the 40-bit register, and a 32-bit store
/// drops bits 7:0 (PRM 4.06). F0 = 1.0, F1 = 3 * 2^-25.
#[test]
fn prm_rnd32_rounds_at_24_bits_and_40_bit_mode_keeps_the_extension() {
    let f: [FloatReg; 16] = std::array::from_fn(|n| FloatReg::new(n as u8).unwrap());
    let code = leaf([
        Instruction48::FloatAdd {
            dst: f[2],
            lhs: f[0],
            rhs: f[1],
        },
        Instruction48::FloatSubtract {
            dst: f[3],
            lhs: f[2],
            rhs: f[0],
        },
        Instruction48::DmDirectStore {
            src: DataReg::new(2).unwrap(),
            address: DmWordAddress32::new(0x10),
        },
    ]);
    let mut start = ScalarState::default();
    start.data[0] = 1.0f32.to_bits();
    start.data[1] = 0x33c0_0000;
    let (rnd32, extension, stored) = run_prm(&code, &start);
    assert_eq!(
        (rnd32.data[2], extension[2], rnd32.data[3], stored),
        (0x3f80_0001, 0, 0x3400_0000, 0x3f80_0001)
    );
    assert_eq!(run_host(&code, &start).data[2..4], rnd32.data[2..4]);

    let start = ScalarState { mode1: 0, ..start };
    let (extended, extension, stored) = run_prm(&code, &start);
    assert_eq!(
        (extended.data[2], extension[2], extended.data[3], stored),
        (0x3f80_0000, 0xc0, 0x33c0_0000, 0x3f80_0000)
    );
    // Host f32 has no 40-bit model, so it refuses the reset default.
    let mut memory = DmMemory::new(DmWordAddress32::new(0x10), vec![0]).unwrap();
    assert_eq!(
        run_scalar(&code, &mut start.clone(), &mut memory, 64),
        Err(SimulationError::Modes(0))
    );
}

/// PRM 23.19: FLOAT rounds at the 40-bit boundary even under RND32. RND32 then
/// clears bits 7:0 of the FIX operand (truncation); only the 40-bit default gets
/// the integer back. Host f32 rounds 2^31 - 1 up to 2^31, an overflow for FIX.
#[test]
fn prm_float_keeps_40_bits_even_under_rnd32() {
    let code = leaf([
        Instruction48::FloatFromInteger {
            dst: FloatReg::new(2).unwrap(),
            src: DataReg::new(0).unwrap(),
        },
        Instruction48::FixToInteger {
            dst: DataReg::new(3).unwrap(),
            src: FloatReg::new(2).unwrap(),
        },
    ]);
    let mut start = ScalarState::default();
    start.data[0] = 0x7fff_ffff;
    let (rnd32, extension, _) = run_prm(&code, &start);
    assert_eq!(
        (rnd32.data[2], extension[2], rnd32.data[3], extension[3]),
        (0x4eff_ffff, 0xfe, 0x7fff_ff80, 0)
    );
    let (extended, extension, _) = run_prm(
        &code,
        &ScalarState {
            mode1: 0,
            ..start.clone()
        },
    );
    assert_eq!(
        (extended.data[2], extension[2], extended.data[3]),
        (0x4eff_ffff, 0xfe, 0x7fff_ffff)
    );
    let host = run_host(&code, &start);
    assert_eq!((host.data[2], host.data[3]), (0x4f00_0000, u32::MAX));
}

/// PRM 23.02/.03/.07, 26.2 and 5.84: a NaN operand, infinity minus infinity and
/// infinity times zero return all ones. Overflow and infinite operands give
/// ±infinity. COMP with a NaN operand clears AZ.
#[test]
fn prm_nan_and_infinity_follow_the_prm() {
    let f: [FloatReg; 16] = std::array::from_fn(|n| FloatReg::new(n as u8).unwrap());
    let code = leaf([
        Instruction48::FloatAdd {
            dst: f[2],
            lhs: f[0],
            rhs: f[1],
        },
        Instruction48::FloatAdd {
            dst: f[3],
            lhs: f[4],
            rhs: f[5],
        },
        Instruction48::FloatMultiply {
            dst: f[6],
            lhs: f[4],
            rhs: f[7],
        },
        Instruction48::FloatSubtract {
            dst: f[8],
            lhs: f[5],
            rhs: f[1],
        },
        Instruction48::FloatMultiply {
            dst: f[9],
            lhs: f[10],
            rhs: f[10],
        },
        // 1 - 1 sets AZ, so the compare must clear it.
        Instruction48::FloatSubtract {
            dst: f[11],
            lhs: f[1],
            rhs: f[1],
        },
        Instruction48::FloatCompare {
            lhs: f[0],
            rhs: f[0],
        },
    ]);
    let mut start = ScalarState::default();
    start.data[0] = 0x7fc0_0000; // quiet NaN
    start.data[1] = 1.0f32.to_bits();
    start.data[4] = 0x7f80_0000; // +infinity
    start.data[5] = 0xff80_0000; // -infinity
    start.data[10] = 0x5f80_0000; // 2^64
    for mode1 in [MODE1_RND32, 0] {
        let (prm, ..) = run_prm(
            &code,
            &ScalarState {
                mode1,
                ..start.clone()
            },
        );
        assert_eq!(
            [
                prm.data[2],
                prm.data[3],
                prm.data[6],
                prm.data[8],
                prm.data[9]
            ],
            [u32::MAX, u32::MAX, u32::MAX, 0xff80_0000, 0x7f80_0000],
            "MODE1 {mode1:#x}"
        );
        assert!(!prm.alu_zero);
    }
    let host = run_host(&code, &start).data[2];
    assert!(f32::from_bits(host).is_nan() && host != u32::MAX);
}

/// PRM 5.84: under RND32 a result equals the IEEE single result, except for the
/// denormal flush. So for normal operands the PRM path equals host f32, but host
/// subnormal results become signed zero (a host MIN_NORMAL result may too).
/// Pseudo-random operands cover cancellation, rounding ties and both exponent ends.
#[test]
fn prm_rnd32_matches_host_f32_for_normal_operands() {
    let f: [FloatReg; 16] = std::array::from_fn(|n| FloatReg::new(n as u8).unwrap());
    let code = leaf([
        Instruction48::FloatMultiply {
            dst: f[2],
            lhs: f[0],
            rhs: f[1],
        },
        Instruction48::FloatAdd {
            dst: f[3],
            lhs: f[0],
            rhs: f[1],
        },
        Instruction48::FloatSubtract {
            dst: f[4],
            lhs: f[0],
            rhs: f[1],
        },
    ]);
    let mut seed = 0x2545_f491_4f6c_dd1d_u64;
    let mut next = || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        (seed >> 32) as u32
    };
    for _ in 0..20_000 {
        let x_exponent = 1 + next() % 254;
        // Every other pair has close exponents: cancellation and rounding ties.
        let y_exponent = if next() & 1 == 0 {
            1 + next() % 254
        } else {
            (x_exponent + next() % 5).saturating_sub(2).clamp(1, 254)
        };
        let mut start = ScalarState::default();
        start.data[0] = (next() & 0x807f_ffff) | (x_exponent << 23);
        start.data[1] = (next() & 0x807f_ffff) | (y_exponent << 23);
        let (prm, ..) = run_prm(&code, &start);
        let host = run_host(&code, &start);
        for n in 2..=4 {
            let (expected, actual) = (host.data[n], prm.data[n]);
            let operands = (start.data[0], start.data[1]);
            match expected & !F_SIGN {
                ..0x0080_0000 => assert_eq!(actual, expected & F_SIGN, "F{n} {operands:x?}"),
                0x0080_0000 => assert!(
                    actual == expected || actual == expected & F_SIGN,
                    "F{n} {operands:x?}"
                ),
                _ => assert_eq!(actual, expected, "F{n} {operands:x?}"),
            }
        }
    }
}

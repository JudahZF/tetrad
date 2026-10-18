#![forbid(unsafe_code)]

use tetrad_capture::filters::emitted::high_pass::{CompiledHighPass, Error, Layout};
use tetrad_capture::filters::{V1HighPassCoefficients, V1HighPassReference};
use tetrad_sharc::InstructionWord48;
use tetrad_sharc::address::{DmWordAddress32, PmWordAddress24};
use tetrad_sharc::isa48::{MODE1_CBUFEN, MODE1_RND32, MODE1_SCALAR_CLEAR};
use tetrad_sharc::simulator::{DmMemory, ScalarState, SimulationError};

const INPUT: u32 = 100;
const OUTPUT: u32 = 140;
const COEFFICIENT: u32 = 180;
const HISTORY: u32 = 190;

fn asymmetric() -> V1HighPassCoefficients {
    V1HighPassCoefficients {
        feedforward: [0.37, -0.219, 0.08],
        feedback: [-0.31, 0.11],
    }
}

fn identity() -> V1HighPassCoefficients {
    V1HighPassCoefficients {
        feedforward: [1.0, 0.0, 0.0],
        feedback: [0.0, 0.0],
    }
}

fn compiled(coefficients: V1HighPassCoefficients) -> CompiledHighPass {
    CompiledHighPass::compile(
        PmWordAddress24::new(0x1200).unwrap(),
        Layout::new(INPUT, OUTPUT, COEFFICIENT, HISTORY).unwrap(),
        coefficients,
    )
    .unwrap()
}

fn fixture(
    code: &CompiledHighPass,
    history: [f32; 2],
    inputs: &[f32; 32],
) -> (ScalarState, DmMemory) {
    let mut words = vec![0x5a5a_5a5a; 110];
    words[..32].copy_from_slice(&inputs.map(f32::to_bits));
    words[80..85].copy_from_slice(&code.coefficient_bits());
    words[90..92].copy_from_slice(&history.map(f32::to_bits));
    let mut memory = DmMemory::new(DmWordAddress32::new(INPUT), words).unwrap();
    // Existing nonfinite data and prior write history outside the four spans are legal.
    memory
        .write(DmWordAddress32::new(209), f32::NAN.to_bits())
        .unwrap();
    let state = ScalarState {
        data: [0x1234_5678; 16],
        indices: [73; 8],
        modifiers: [-7; 8],
        bases: [88; 8],
        lengths: [9; 8],
        mode1: MODE1_RND32 | MODE1_CBUFEN | (1 << 12),
        alu_zero: true,
    };
    (state, memory)
}

fn assert_reference(
    code: &CompiledHighPass,
    reference: &mut V1HighPassReference,
    state: &mut ScalarState,
    memory: &mut DmMemory,
    inputs: [f32; 32],
) {
    for (index, input) in inputs.iter().enumerate() {
        memory
            .write(DmWordAddress32::new(INPUT + index as u32), input.to_bits())
            .unwrap();
    }
    let expected = reference.process_block(&inputs).unwrap();
    let old_log = memory.writes().to_vec();
    let result = code
        .simulate_atomic(state, memory, code.max_steps())
        .unwrap();
    assert_eq!((result.steps, result.stores), (424, 96));
    assert_eq!(&memory.writes()[..old_log.len()], old_log);
    for (index, value) in expected.iter().enumerate() {
        assert_eq!(
            memory
                .read(DmWordAddress32::new(OUTPUT + index as u32))
                .unwrap(),
            value.to_bits()
        );
    }
    for (slot, value) in reference.state().iter().enumerate() {
        assert_eq!(
            memory
                .read(DmWordAddress32::new(HISTORY + slot as u32))
                .unwrap(),
            value.to_bits()
        );
    }
}

#[test]
fn accumulated_recurrence_matches_host_bits_across_blocks_and_identity_transient() {
    let inputs = std::array::from_fn(|index| (index as f32 - 15.0) / 17.0);
    for coefficients in [asymmetric(), identity()] {
        let code = compiled(coefficients);
        let initial = [0.43, -0.17];
        let (mut state, mut memory) = fixture(&code, initial, &inputs);
        let mut reference = V1HighPassReference::from_state(coefficients, initial).unwrap();
        assert_reference(&code, &mut reference, &mut state, &mut memory, inputs);
        assert_reference(
            &code,
            &mut reference,
            &mut state,
            &mut memory,
            std::array::from_fn(|index| {
                if index == 0 {
                    -0.0
                } else {
                    index as f32 / 31.0
                }
            }),
        );
        if coefficients == identity() {
            assert_eq!(reference.state().map(f32::to_bits), [0, 0]);
        }
    }
}

#[test]
fn every_word_serializes_relocates_and_preserves_leaf_abi_canaries() {
    let code = compiled(asymmetric());
    let mut expected = Vec::new();
    for slot in 0..5_u64 {
        expected.push(0x1000_0000_0000 | ((slot + 4) << 32) | (u64::from(COEFFICIENT) + slot));
    }
    for slot in 0..2_u64 {
        expected.push(0x1000_0000_0000 | ((slot + 1) << 32) | (u64::from(HISTORY) + slot));
    }
    // Independent scalar field calculation, with an explicit nine-operation schedule.
    let compute = |op: u64, dst: u64, lhs: u64, rhs: u64| {
        0x013e_0000_0000 | (op << 12) | (dst << 8) | (lhs << 4) | rhs
    };
    for sample in 0..32_u64 {
        expected.push(0x1000_0000_0000 | (u64::from(INPUT) + sample));
        expected.extend([
            compute(0x130, 9, 4, 0),
            compute(0x081, 3, 9, 1),
            compute(0x130, 9, 5, 0),
            compute(0x081, 9, 9, 2),
            compute(0x130, 10, 7, 3),
            compute(0x082, 1, 9, 10),
            compute(0x130, 9, 6, 0),
            compute(0x130, 10, 8, 3),
            compute(0x082, 2, 9, 10),
            0x1103_0000_0000 | (u64::from(OUTPUT) + sample),
            0x1101_0000_0000 | u64::from(HISTORY),
            0x1102_0000_0000 | u64::from(HISTORY + 1),
        ]);
    }
    expected.push(0x0a3e_0000_0000);
    assert_eq!(expected.len(), 424);
    assert_eq!(
        code.code()
            .words()
            .iter()
            .map(|word| word.bits())
            .collect::<Vec<_>>(),
        expected
    );
    let bytes = code.code().isa48_le_bytes();
    assert_eq!(bytes.len(), 424 * 6);
    for (chunk, bits) in bytes.chunks_exact(6).zip(expected) {
        assert_eq!(
            InstructionWord48::from_le_bytes(chunk.try_into().unwrap()).bits(),
            bits
        );
    }
    let last_origin = 0xff_ffff - 423;
    let relocated = CompiledHighPass::compile(
        PmWordAddress24::new(last_origin).unwrap(),
        code.layout(),
        asymmetric(),
    )
    .unwrap();
    assert_eq!(relocated.code().origin().get(), last_origin);
    assert_eq!(relocated.code().words(), code.code().words());
    assert!(
        CompiledHighPass::compile(
            PmWordAddress24::new(last_origin + 1).unwrap(),
            code.layout(),
            asymmetric()
        )
        .is_err()
    );
    assert!(PmWordAddress24::new(0x100_0000).is_err());
    let (mut state, mut memory) = fixture(&code, [0.1, -0.1], &[0.25; 32]);
    let before_state = state.clone();
    let before_words = memory.words().to_vec();
    code.simulate_atomic(&mut state, &mut memory, 424).unwrap();
    assert_eq!(&state.data[11..], &before_state.data[11..]);
    assert_eq!(state.indices, before_state.indices);
    assert_eq!(state.modifiers, before_state.modifiers);
    assert_eq!(state.bases, before_state.bases);
    assert_eq!(state.lengths, before_state.lengths);
    assert_eq!(state.mode1, before_state.mode1);
    assert_ne!(&state.data[..11], &before_state.data[..11]);
    for (offset, (after, before)) in memory.words().iter().zip(before_words).enumerate() {
        if !(40..72).contains(&offset) && !(90..92).contains(&offset) {
            assert_eq!(*after, before);
        }
    }
    for sample in 0..32_usize {
        assert_eq!(
            &memory.writes()[1 + sample * 3..1 + sample * 3 + 3],
            [OUTPUT + sample as u32, HISTORY, HISTORY + 1].map(DmWordAddress32::new)
        );
    }
}

#[test]
fn all_six_dm_overlap_pairs_endpoints_and_nonfinite_coefficients_are_rejected() {
    let starts = [INPUT, OUTPUT, COEFFICIENT, HISTORY];
    for first in 0..4 {
        for second in first + 1..4 {
            let mut invalid = starts;
            invalid[second] = invalid[first];
            assert!(matches!(
                Layout::new(invalid[0], invalid[1], invalid[2], invalid[3]),
                Err(Error::Overlap { .. })
            ));
        }
    }
    assert!(Layout::new(0, 32, 64, 69).is_ok()); // Adjacent is disjoint.
    for (index, length) in [32, 32, 5, 2].into_iter().enumerate() {
        let mut high = [0, 32, 64, 69];
        high[index] = u32::MAX - length;
        assert!(Layout::new(high[0], high[1], high[2], high[3]).is_ok());
        high[index] += 1;
        assert!(matches!(
            Layout::new(high[0], high[1], high[2], high[3]),
            Err(Error::Address(_))
        ));
    }
    for (slot, invalid) in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY]
        .into_iter()
        .enumerate()
    {
        let mut coefficients = asymmetric();
        coefficients.feedforward[slot] = invalid;
        assert!(matches!(
            CompiledHighPass::compile(
                PmWordAddress24::new(0).unwrap(),
                Layout::new(INPUT, OUTPUT, COEFFICIENT, HISTORY).unwrap(),
                coefficients
            ),
            Err(Error::NonFiniteCoefficient { .. })
        ));
    }
    let mut coefficients = asymmetric();
    coefficients.feedback[1] = f32::NAN;
    assert!(
        CompiledHighPass::compile(
            PmWordAddress24::new(0).unwrap(),
            Layout::new(INPUT, OUTPUT, COEFFICIENT, HISTORY).unwrap(),
            coefficients
        )
        .is_err()
    );
    assert!(
        CompiledHighPass::compile(
            PmWordAddress24::new(0).unwrap(),
            Layout::new(INPUT, OUTPUT, COEFFICIENT, HISTORY).unwrap(),
            V1HighPassCoefficients {
                feedforward: [f32::MAX, -0.0, f32::from_bits(1)],
                feedback: [123.0, -9.0]
            }
        )
        .is_ok()
    );
}

fn assert_rollback(
    code: &CompiledHighPass,
    mut state: ScalarState,
    mut memory: DmMemory,
    budget: usize,
) -> Error {
    let before_state = state.clone();
    let before_memory = memory.clone();
    let error = code
        .simulate_atomic(&mut state, &mut memory, budget)
        .unwrap_err();
    assert_eq!(state, before_state);
    assert_eq!(memory, before_memory); // Includes every word and prior write history.
    error
}

#[test]
fn atomic_failures_keep_registers_all_memory_and_prior_log_even_after_late_overflow() {
    let code = compiled(asymmetric());
    let (state, memory) = fixture(&code, [0.1, -0.1], &[0.25; 32]);
    for mode1 in [0, MODE1_RND32 | MODE1_SCALAR_CLEAR] {
        let mut bad = state.clone();
        bad.mode1 = mode1;
        assert!(matches!(
            assert_rollback(&code, bad, memory.clone(), 424),
            Error::Modes(_)
        ));
    }
    // Put each selected span last; only its final word is absent. All other
    // spans are complete, so each distinct presence check is exercised.
    for (missing, length) in [32, 32, 5, 2].into_iter().enumerate() {
        let mut starts = [0, 32, 64, 69];
        starts[missing] = 100;
        let layout = Layout::new(starts[0], starts[1], starts[2], starts[3]).unwrap();
        let missing_code =
            CompiledHighPass::compile(PmWordAddress24::new(0).unwrap(), layout, asymmetric())
                .unwrap();
        let absent = DmMemory::new(
            DmWordAddress32::new(0),
            vec![0; (100 + length - 1) as usize],
        )
        .unwrap();
        assert!(
            matches!(assert_rollback(&missing_code, state.clone(), absent, 424),
            Error::Simulation(SimulationError::MemoryBounds { address, words }) if address == 100 && words == length)
        );
    }
    let mut mismatch = memory.clone();
    mismatch
        .write(DmWordAddress32::new(COEFFICIENT), 0)
        .unwrap();
    assert!(matches!(
        assert_rollback(&code, state.clone(), mismatch, 424),
        Error::CoefficientBits { slot: 0 }
    ));
    for address in [INPUT + 31, HISTORY + 1] {
        let mut bad = memory.clone();
        bad.write(DmWordAddress32::new(address), f32::NAN.to_bits())
            .unwrap();
        assert!(matches!(
            assert_rollback(&code, state.clone(), bad, 424),
            Error::NonFiniteMemory { .. }
        ));
    }
    for budget in [0, 7, 423] {
        assert!(
            matches!(assert_rollback(&code, state.clone(), memory.clone(), budget), Error::Simulation(SimulationError::StepBudget(value)) if value == budget)
        );
    }
    // One overflowing case for each arithmetic position; input becomes large only
    // late in the block. p1/t1/q1 and p2/q2 overflow must not be masked by later writes.
    let cases = [
        (
            V1HighPassCoefficients {
                feedforward: [f32::MAX, 0.0, 0.0],
                feedback: [0.0, 0.0],
            },
            [0.0, 0.0],
        ),
        (
            V1HighPassCoefficients {
                feedforward: [1.0, 1.0, 0.0],
                feedback: [0.0, 0.0],
            },
            [0.0, 0.0],
        ),
        (
            V1HighPassCoefficients {
                feedforward: [0.0, f32::MAX, 0.0],
                feedback: [0.0, 0.0],
            },
            [0.0, 0.0],
        ),
        (
            V1HighPassCoefficients {
                feedforward: [0.0, 1.0, 1.0],
                feedback: [0.0, 0.0],
            },
            [0.0, 0.0],
        ),
        (
            V1HighPassCoefficients {
                feedforward: [1.0, 0.0, 0.0],
                feedback: [f32::MAX, 0.0],
            },
            [0.0, 0.0],
        ),
        (
            V1HighPassCoefficients {
                feedforward: [1.0, 1.0, 0.0],
                feedback: [-1.0, 0.0],
            },
            [0.0, 0.0],
        ),
        (
            V1HighPassCoefficients {
                feedforward: [0.0, 0.0, f32::MAX],
                feedback: [0.0, 0.0],
            },
            [0.0, 0.0],
        ),
        (
            V1HighPassCoefficients {
                feedforward: [1.0, 0.0, 0.0],
                feedback: [0.0, f32::MAX],
            },
            [0.0, 0.0],
        ),
        (
            V1HighPassCoefficients {
                feedforward: [1.0, 0.0, 1.0],
                feedback: [0.0, -1.0],
            },
            [0.0, 0.0],
        ),
    ];
    for (coefficients, history) in cases {
        let overflow_code = compiled(coefficients);
        let mut inputs = [0.0; 32];
        inputs[30] = f32::MAX;
        inputs[31] = f32::MAX;
        let (bad_state, bad_memory) = fixture(&overflow_code, history, &inputs);
        let mut reference = V1HighPassReference::from_state(coefficients, history).unwrap();
        assert!(reference.process_block(&inputs).is_err());
        assert!(matches!(
            assert_rollback(&overflow_code, bad_state, bad_memory, 424),
            Error::NonFiniteMemory { .. }
        ));
    }
    // State overflow on the last sample is visible even without a following output.
    let overflow_code = compiled(V1HighPassCoefficients {
        feedforward: [0.0, 0.0, f32::MAX],
        feedback: [0.0, 0.0],
    });
    let mut inputs = [0.0; 32];
    inputs[31] = 2.0;
    let (s, m) = fixture(&overflow_code, [0.0, 0.0], &inputs);
    assert!(
        matches!(assert_rollback(&overflow_code, s, m, 424), Error::NonFiniteMemory { address } if address.get() == HISTORY + 1)
    );
}

#[test]
fn owned_coefficient_copy_requires_exact_reload_and_explicit_history_policy() {
    let mut source = asymmetric();
    let first = compiled(source);
    let pinned = first.coefficient_bits();
    source.feedforward[0] = -0.0;
    assert_eq!(first.coefficient_bits(), pinned);
    let (mut state, mut memory) = fixture(&first, [0.3, -0.2], &[0.125; 32]);
    first.simulate_atomic(&mut state, &mut memory, 424).unwrap();
    let old_history = [0, 1]
        .map(|slot| f32::from_bits(memory.read(DmWordAddress32::new(HISTORY + slot)).unwrap()));
    let second = compiled(identity());
    assert!(matches!(
        assert_rollback(&second, state.clone(), memory.clone(), 424),
        Error::CoefficientBits { .. }
    ));
    for (slot, bits) in second.coefficient_bits().iter().enumerate() {
        memory
            .write(DmWordAddress32::new(COEFFICIENT + slot as u32), *bits)
            .unwrap();
    }
    let mut reset_memory = memory.clone();
    for slot in 0..2 {
        reset_memory
            .write(DmWordAddress32::new(HISTORY + slot), 0)
            .unwrap();
    }
    let mut reset_state = state.clone();
    let mut preserved = V1HighPassReference::from_state(identity(), old_history).unwrap();
    let mut reset = V1HighPassReference::new(identity()).unwrap();
    assert_reference(
        &second,
        &mut preserved,
        &mut state,
        &mut memory,
        [0.125; 32],
    );
    assert_reference(
        &second,
        &mut reset,
        &mut reset_state,
        &mut reset_memory,
        [0.125; 32],
    );
    assert_ne!(
        memory.read(DmWordAddress32::new(OUTPUT)).unwrap(),
        reset_memory.read(DmWordAddress32::new(OUTPUT)).unwrap()
    );
    // Signed zero is finite but coefficient matching is bit-exact.
    let zero_code = compiled(source);
    let (s, mut m) = fixture(&zero_code, [0.0, 0.0], &[0.0; 32]);
    m.write(DmWordAddress32::new(COEFFICIENT), 0.0_f32.to_bits())
        .unwrap();
    assert!(matches!(
        assert_rollback(&zero_code, s, m, 424),
        Error::CoefficientBits { slot: 0 }
    ));
}

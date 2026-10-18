use tetrad_sharc::address::{AddressError, DmWordAddress32, PmWordAddress24};
use tetrad_sharc::emit::EmitError;
use tetrad_sharc::fir::{
    CompiledFir, FIR_ABI, FIR_OUTPUT_SAMPLES, FIR_WORD_COUNT, FirError, FirLayout, FirLength,
    compile_fir, simulate_fir,
};
use tetrad_sharc::isa48::{MODE1_RND32, MODE1_SCALAR_CLEAR};
use tetrad_sharc::simulator::{DmMemory, ScalarState, SimulationError, run_scalar};

const CANARY: u32 = 0x7fc0_0123;
const LENGTHS: [FirLength; 2] = [FirLength::Fir512, FirLength::Fir2048];

fn fixture(length: FirLength) -> CompiledFir {
    let n = length.taps();
    let layout = FirLayout::new(
        length,
        DmWordAddress32::new(0x1001),
        DmWordAddress32::new(0x1001 + n + 32),
        DmWordAddress32::new(0x1001 + 2 * n + 33),
    )
    .unwrap();
    compile_fir(PmWordAddress24::new(0x123456).unwrap(), layout).unwrap()
}

fn state() -> ScalarState {
    ScalarState {
        data: std::array::from_fn(|i| 0xdead_0000 + i as u32),
        indices: std::array::from_fn(|i| 0xbeef_0000 + i as u32),
        modifiers: std::array::from_fn(|i| -100 - i as i32),
        bases: std::array::from_fn(|i| 0xcafe_0000 + i as u32),
        lengths: [0, 0, 19, 23, 29, 31, 37, 41],
        mode1: MODE1_RND32 | (1 << 24), // CBUFEN is harmless with L0=L1=0.
        alu_zero: false,
    }
}

fn memory(fir: &CompiledFir, inputs: &[f32], taps: &[f32]) -> DmMemory {
    let layout = fir.layout();
    assert_eq!(inputs.len(), layout.input_words() as usize);
    assert_eq!(taps.len(), layout.length().taps() as usize);
    let base = layout.input().get() - 1;
    let mut words = vec![CANARY; (layout.output().get() + 33 - base) as usize];
    let input_offset = (layout.input().get() - base) as usize;
    let taps_offset = (layout.taps().get() - base) as usize;
    for (dst, value) in words[input_offset..].iter_mut().zip(inputs) {
        *dst = value.to_bits();
    }
    for (dst, value) in words[taps_offset..].iter_mut().zip(taps) {
        *dst = value.to_bits();
    }
    let mut memory = DmMemory::new(DmWordAddress32::new(base), words).unwrap();
    // A pre-existing write must survive both success and rollback.
    memory.write(memory.base(), CANARY).unwrap();
    memory
}

// Direct causal indexing is independent of the compiler's pointer walk.
fn direct(inputs: &[f32], taps: &[f32]) -> Vec<u32> {
    (0..32)
        .map(|sample| {
            let current = taps.len() - 1 + sample;
            let mut sum = 0.0f32;
            for tap in (0..taps.len()).rev() {
                let product = inputs[current - tap] * taps[tap];
                sum += product;
            }
            sum.to_bits()
        })
        .collect()
}

fn outputs(fir: &CompiledFir, memory: &DmMemory) -> Vec<u32> {
    (0..FIR_OUTPUT_SAMPLES)
        .map(|sample| {
            memory
                .read(fir.layout().output().checked_add(sample).unwrap())
                .unwrap()
        })
        .collect()
}

fn assert_preserved(fir: &CompiledFir, before: &ScalarState, after: &ScalarState) {
    for index in 0..16 {
        if !FIR_ABI
            .clobbered_data
            .iter()
            .any(|r| usize::from(r.index()) == index)
        {
            assert_eq!(after.data[index], before.data[index]);
        }
    }
    assert_eq!(after.indices[2..], before.indices[2..]);
    assert_eq!(after.modifiers[2..], before.modifiers[2..]);
    assert_eq!(after.bases, before.bases);
    assert_eq!(after.lengths, before.lengths);
    assert_eq!(after.mode1, before.mode1);
    assert_eq!(
        after.indices[0],
        fir.layout().input().get() + fir.layout().input_words()
    );
    assert_eq!(after.indices[1], fir.layout().taps().get() - 1);
    assert_eq!(after.modifiers[..2], [1, -1]);
    assert_eq!(after.data[9], 0);
    assert!(after.alu_zero);
}

fn assert_only_outputs_written(fir: &CompiledFir, before: &DmMemory, after: &DmMemory) {
    let output = (fir.layout().output().get() - before.base().get()) as usize;
    assert_eq!(after.words()[..output], before.words()[..output]);
    assert_eq!(after.words()[output + 32..], before.words()[output + 32..]);
    let expected: Vec<_> = before
        .writes()
        .iter()
        .copied()
        .chain((0..32).map(|s| fir.layout().output().checked_add(s).unwrap()))
        .collect();
    assert_eq!(after.writes(), expected);
}

#[test]
fn fixed_image_words_addresses_counts_and_pm_limits() {
    for length in LENGTHS {
        let fir = fixture(length);
        let n = u64::from(length.taps());
        assert_eq!(fir.code().word_count(), FIR_WORD_COUNT);
        assert_eq!(fir.code().isa48_le_bytes().len(), 2130);
        assert_eq!(
            fir.executed_steps(),
            if n == 512 { 98_467 } else { 393_379 }
        );
        let words: Vec<_> = fir.code().words().iter().map(|w| w.bits()).collect();
        assert_eq!(words[..2], [0x0f20_0000_0001, 0x0f21_ffff_ffff]);
        for sample in 0..32usize {
            let offset = 2 + sample * 11;
            assert_eq!(
                words[offset..offset + 11],
                [
                    0x0f10_0000_0000 | (u64::from(fir.layout().input().get()) + sample as u64),
                    0x0f11_0000_0000 | (u64::from(fir.layout().taps().get()) + n - 1),
                    0x0f01_0000_0000,
                    0x0f09_0000_0000 | n,
                    0x503e_0000_0000,
                    0x527e_0200_0000,
                    0x013e_0013_0004,
                    0x013e_0008_1110,
                    0x013e_0002_a990,
                    0x0720_00ff_fffb,
                    0x1101_0000_0000 | (u64::from(fir.layout().output().get()) + sample as u64),
                ]
            );
            assert_eq!(
                fir.code().symbols()
                    [&tetrad_sharc::Label::new(format!("fir.output.{sample}.loop"))]
                    .get(),
                fir.code().origin().get() + offset as u32 + 4,
            );
        }
        assert_eq!(words[354], 0x0a3e_0000_0000);
        let last_origin = 0xff_ffff - (FIR_WORD_COUNT as u32 - 1);
        assert!(compile_fir(PmWordAddress24::new(last_origin).unwrap(), fir.layout()).is_ok());
        assert!(matches!(
            compile_fir(PmWordAddress24::new(last_origin + 1).unwrap(), fir.layout()),
            Err(FirError::Emit(EmitError::ProgramBounds { .. }))
        ));
        assert!(compile_fir(PmWordAddress24::new(0).unwrap(), fir.layout()).is_ok());
    }
}

#[test]
fn layout_checks_all_overlap_pairs_and_representable_endpoints() {
    for length in LENGTHS {
        let n = length.taps();
        let make = |a, b, c| {
            FirLayout::new(
                length,
                DmWordAddress32::new(a),
                DmWordAddress32::new(b),
                DmWordAddress32::new(c),
            )
        };
        for (a, b, c) in [
            (100, 101, 10000),
            (101, 100, 10000),
            (100, 10000, 101),
            (101, 10000, 100),
            (10000, 100, 101),
            (10000, 101, 100),
            (100, 100, 10000),
            (100, 10000, 100),
            (10000, 100, 100),
        ] {
            assert_eq!(make(a, b, c), Err(FirError::DmOverlap));
        }
        assert!(make(1, n + 32, 2 * n + 32).is_ok()); // Adjacent exclusive ends.
        assert!(make(n + 33, 1, n + 1).is_ok()); // Placement need not be ordered.
        assert_eq!(make(10000, 0, 20000), Err(FirError::TapBaseZero));
        for (a, b, c) in [
            (u32::MAX - n - 30, 1, 5000),
            (1, u32::MAX - n + 1, 5000),
            (1, 5000, u32::MAX - 31),
        ] {
            assert_eq!(
                make(a, b, c),
                Err(FirError::Address(AddressError::DmOverflow))
            );
        }
        for (a, b, c) in [
            (u32::MAX - n - 31, 1, 5000),
            (1, u32::MAX - n, 5000),
            (1, 5000, u32::MAX - 32),
        ] {
            assert!(make(a, b, c).is_ok());
        }
        // Final descending I1=0 is representable and need not be in supplied DM.
        let layout = make(n + 33, 1, n + 1).unwrap();
        let fir = compile_fir(PmWordAddress24::new(0).unwrap(), layout).unwrap();
        let mut dm =
            DmMemory::new(DmWordAddress32::new(1), vec![0; (2 * n + 63) as usize]).unwrap();
        let mut state = state();
        simulate_fir(&fir, &mut state, &mut dm, fir.executed_steps()).unwrap();
        assert_eq!(state.indices[1], 0);
        assert_eq!(state.indices[0], 2 * n + 64); // One-past supplied DM.
    }
}

#[test]
fn descending_direct_f32_asymmetry_cancellation_and_preservation() {
    for length in LENGTHS {
        let fir = fixture(length);
        let n = length.taps() as usize;
        let mut taps: Vec<_> = (0..n)
            .map(|i| ((i * 17 % 43) as f32 - 21.0) / 32.0)
            .collect();
        let mut inputs: Vec<_> = (0..n + 31)
            .map(|i| ((i * 13 % 37) as f32 - 18.0) / 16.0)
            .collect();
        for cancellation in [false, true] {
            if cancellation {
                taps.fill(0.0);
                taps[..3].copy_from_slice(&[1.0, 16_777_216.0, -16_777_216.0]);
                inputs.fill(1.0);
            }
            let expected = direct(&inputs, &taps);
            if cancellation {
                assert_eq!(expected, vec![1.0f32.to_bits(); 32]); // Ascending gives zero.
            }
            let mut dm = memory(&fir, &inputs, &taps);
            let before_dm = dm.clone();
            let mut state = state();
            let before_state = state.clone();
            let outcome = simulate_fir(&fir, &mut state, &mut dm, fir.executed_steps()).unwrap();
            assert_eq!(outcome.steps, fir.executed_steps());
            assert_eq!(outcome.stores, 32);
            assert_eq!(outputs(&fir, &dm), expected);
            assert_preserved(&fir, &before_state, &state);
            assert_only_outputs_written(&fir, &before_dm, &dm);
        }
    }
}

#[test]
fn separate_multiply_add_rounding_is_not_fused() {
    for length in LENGTHS {
        let fir = fixture(length);
        let n = length.taps() as usize;
        let mut taps = vec![0.0; n];
        taps[n - 1] = -1.0;
        taps[n - 2] = f32::from_bits(0x3f80_0001);
        let mut inputs = vec![0.0; n + 31];
        inputs[0] = 1.0;
        inputs[1] = f32::from_bits(0x3f7f_fffe);
        assert_ne!(taps[n - 2].mul_add(inputs[1], -1.0), 0.0);
        let expected = direct(&inputs, &taps);
        assert_eq!(expected[0], 0.0f32.to_bits());
        let mut dm = memory(&fir, &inputs, &taps);
        simulate_fir(&fir, &mut state(), &mut dm, fir.executed_steps()).unwrap();
        assert_eq!(outputs(&fir, &dm), expected);
    }
}

#[test]
fn chronological_windows_across_callbacks_and_independent_instances() {
    for length in LENGTHS {
        let fir = fixture(length);
        let n = length.taps() as usize;
        let mut taps = vec![0.0; n];
        for (tap, value) in [(0, 2.0), (1, -3.0), (31, 5.0), (32, 7.0), (n - 1, 11.0)] {
            taps[tap] = value;
        }
        let stream: Vec<_> = (0..n + 63)
            .map(|i| ((i * 11 % 31) as f32 - 15.0) / 8.0)
            .collect();
        let mut state_a = state();
        let mut state_b = state();
        let mut instance_b = memory(&fir, &vec![0.0; n + 31], &taps);
        let before_b = instance_b.clone();
        let mut all = Vec::new();
        let identity = fir.clone();
        for start in [0, 32] {
            let inputs = &stream[start..start + n + 31];
            let mut dm = memory(&fir, inputs, &taps);
            let before_dm = dm.clone();
            simulate_fir(&fir, &mut state_a, &mut dm, fir.executed_steps()).unwrap();
            assert_eq!(outputs(&fir, &dm), direct(inputs, &taps));
            assert_only_outputs_written(&fir, &before_dm, &dm);
            all.extend(outputs(&fir, &dm));
            assert_eq!(instance_b, before_b);
            assert_eq!(fir, identity);
        }
        let expected: Vec<_> = (0..64)
            .map(|sample| {
                let current = n - 1 + sample;
                let mut sum = 0.0f32;
                for tap in (0..n).rev() {
                    let product = taps[tap] * stream[current - tap];
                    sum += product;
                }
                sum.to_bits()
            })
            .collect();
        assert_eq!(all, expected);
        simulate_fir(&fir, &mut state_b, &mut instance_b, fir.executed_steps()).unwrap();
        assert_eq!(outputs(&fir, &instance_b), vec![0; 32]);
    }
}

#[test]
fn exact_budget_and_one_below_atomic_rollback_but_raw_execution_has_partial_effects() {
    for length in LENGTHS {
        let fir = fixture(length);
        let n = length.taps() as usize;
        let mut dm = memory(&fir, &vec![1.0; n + 31], &vec![1.0; n]);
        let mut state = state();
        let before = (state.clone(), dm.clone());
        assert_eq!(
            simulate_fir(&fir, &mut state, &mut dm, fir.executed_steps() - 1),
            Err(FirError::Simulation(SimulationError::StepBudget(
                fir.executed_steps() - 1
            )))
        );
        assert_eq!((state.clone(), dm.clone()), before);
        simulate_fir(&fir, &mut state, &mut dm, fir.executed_steps()).unwrap();
        assert_eq!(outputs(&fir, &dm), vec![(n as f32).to_bits(); 32]);
        let (mut raw_state, mut raw_dm) = before.clone();
        assert_eq!(
            run_scalar(
                fir.code(),
                &mut raw_state,
                &mut raw_dm,
                fir.executed_steps() - 1
            ),
            Err(SimulationError::StepBudget(fir.executed_steps() - 1))
        );
        assert_ne!((raw_state.clone(), raw_dm.clone()), before);
        assert_eq!(raw_dm.writes().len(), before.1.writes().len() + 32);
        assert_eq!(outputs(&fir, &raw_dm), outputs(&fir, &dm));
    }
}

#[test]
fn stale_modes_lengths_and_supplied_dm_bounds_reject_without_effects() {
    for length in LENGTHS {
        let fir = fixture(length);
        let n = length.taps() as usize;
        let valid_dm = memory(&fir, &vec![1.0; n + 31], &vec![1.0; n]);
        let mut modes = vec![0];
        modes.extend(
            (0..32)
                .filter(|bit| MODE1_SCALAR_CLEAR & (1 << bit) != 0)
                .map(|bit| MODE1_RND32 | (1 << bit)),
        );
        for mode in modes {
            let mut state = state();
            state.mode1 = mode;
            let mut dm = valid_dm.clone();
            let before = (state.clone(), dm.clone());
            assert_eq!(
                simulate_fir(&fir, &mut state, &mut dm, fir.executed_steps()),
                Err(FirError::Simulation(SimulationError::Modes(mode)))
            );
            assert_eq!((state, dm), before);
        }
        for index in [0, 1] {
            let mut state = state();
            state.lengths[index] = 1;
            let mut dm = valid_dm.clone();
            let before = (state.clone(), dm.clone());
            assert_eq!(
                simulate_fir(&fir, &mut state, &mut dm, fir.executed_steps()),
                Err(FirError::Simulation(SimulationError::CircularLength {
                    index,
                    length: 1
                }))
            );
            assert_eq!((state, dm), before);
        }
        for (base, words) in [
            (fir.layout().input().get() + 1, vec![0; 2 * n + 100]),
            (fir.layout().input().get(), vec![0; n + 30]),
            (fir.layout().input().get(), vec![0; 2 * n + 31]),
            (fir.layout().input().get(), vec![0; 2 * n + 64]),
        ] {
            let mut dm = DmMemory::new(DmWordAddress32::new(base), words).unwrap();
            let mut state = state();
            let before = (state.clone(), dm.clone());
            assert!(matches!(
                simulate_fir(&fir, &mut state, &mut dm, fir.executed_steps()),
                Err(FirError::Simulation(SimulationError::MemoryBounds { .. }))
            ));
            assert_eq!((state, dm), before);
        }
    }
}

#[test]
fn nonfinite_operands_and_intermediate_overflow_roll_back_entire_callback() {
    for length in LENGTHS {
        let fir = fixture(length);
        let n = length.taps() as usize;
        for bad in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            for address in [
                fir.layout().input(),
                fir.layout().input().checked_add(n as u32 + 30).unwrap(),
                fir.layout().taps().checked_add(n as u32 - 1).unwrap(),
            ] {
                let mut dm = memory(&fir, &vec![1.0; n + 31], &vec![0.0; n]);
                dm.write(address, bad.to_bits()).unwrap();
                let mut state = state();
                let before = (state.clone(), dm.clone());
                assert_eq!(
                    simulate_fir(&fir, &mut state, &mut dm, fir.executed_steps()),
                    Err(FirError::NonFiniteOperand {
                        address: address.get()
                    })
                );
                assert_eq!((state, dm), before);
            }
        }
        for addition_overflow in [false, true] {
            let mut inputs = vec![1.0; n + 31];
            let mut taps = vec![0.0; n];
            if addition_overflow {
                taps[..3].copy_from_slice(&[-f32::MAX, f32::MAX, f32::MAX]);
            } else {
                inputs[n + 30] = 2.0; // Fail after 31 staged output stores.
                taps[0] = f32::MAX;
            }
            let mut dm = memory(&fir, &inputs, &taps);
            let mut state = state();
            let before = (state.clone(), dm.clone());
            let offset = if addition_overflow {
                9
            } else {
                2 + 31 * 11 + 6
            };
            assert_eq!(
                simulate_fir(&fir, &mut state, &mut dm, fir.executed_steps()),
                Err(FirError::Simulation(SimulationError::NonFiniteResult {
                    pc: fir.code().origin().get() + offset
                }))
            );
            assert_eq!((state, dm), before);
        }
    }
}

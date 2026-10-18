//! Owned raw ISA48 resampling kernels against the owned host reference
//! `V1ResamplingReference` and an independent continuous-time convolution.
//! Synthetic tables only; nothing here is a stock table or stock oracle.

mod emitted {
    use tetrad_capture::filters::HistoryUpdate;
    use tetrad_capture::resampling::emitted::{
        CompiledResampling, EmittedV1Decimator, EmittedV1Interpolator, Error, Kernel, Layout,
        ResamplingLogicalPlacement,
    };
    use tetrad_capture::resampling::{CallerOwnedV1ResamplingTable, V1ResamplingReference};
    use tetrad_sharc::address::{AddressError, DmWordAddress32, PmWordAddress24};
    use tetrad_sharc::emit::EmitError;
    use tetrad_sharc::simulator::{DmMemory, ScalarState, SimulationError};

    const INTERPOLATE_WORDS: usize = 3232;
    const DECIMATE_WORDS: usize = 3200;

    fn table(coefficients: [f32; 16]) -> CallerOwnedV1ResamplingTable {
        CallerOwnedV1ResamplingTable::new(coefficients, "owned synthetic test table".into())
            .unwrap()
    }

    fn placement(origin: u32, dm_base: u32) -> ResamplingLogicalPlacement {
        ResamplingLogicalPlacement {
            origin: PmWordAddress24::new(origin).unwrap(),
            dm_base: DmWordAddress32::new(dm_base),
        }
    }

    fn bits<const N: usize>(values: [f32; N]) -> [u32; N] {
        values.map(f32::to_bits)
    }

    /// Runs both directions over the same blocks and compares every output and
    /// history word bitwise with the host reference.
    fn assert_matches_reference(coefficients: [f32; 16], low: &[f32], high: &[f32]) {
        let mut reference = V1ResamplingReference::new(table(coefficients));
        let mut up =
            EmittedV1Interpolator::new(table(coefficients), placement(0x1000, 0x2000)).unwrap();
        let mut down =
            EmittedV1Decimator::new(table(coefficients), placement(0x8000, 0x2000)).unwrap();
        for block in low.chunks_exact(32) {
            let block = block.try_into().unwrap();
            let expected = reference.interpolate_block(block).unwrap();
            let actual = up.process_block(block, INTERPOLATE_WORDS).unwrap();
            assert_eq!(bits(actual.outputs), bits(expected));
            assert_eq!((actual.receipt.steps, actual.receipt.stores), (3232, 79));
        }
        for block in high.chunks_exact(64) {
            let block = block.try_into().unwrap();
            let expected = reference.decimate_block(block).unwrap();
            let actual = down.process_block(block, DECIMATE_WORDS).unwrap();
            assert_eq!(bits(actual.outputs), bits(expected));
            assert_eq!((actual.receipt.steps, actual.receipt.stores), (3200, 63));
        }
        // The reference history is private; replaying its last inputs is its contract.
        let last_low: [f32; 15] = std::array::from_fn(|k| low[low.len() - 1 - k]);
        let last_high: [f32; 31] = std::array::from_fn(|k| high[high.len() - 1 - k]);
        assert_eq!(bits(up.history()), bits(last_low));
        assert_eq!(bits(down.history()), bits(last_high));
    }

    #[test]
    fn emitted_kernels_match_reference_and_independent_convolution() {
        // Asymmetric dyadic table: every sum below is exact, so an independent
        // zero-extended convolution with no rings or callbacks must agree bitwise.
        let coefficients = std::array::from_fn(|j| (j as f32 - 6.0) / 64.0);
        let low: Vec<f32> = (0..96).map(|n| ((n % 9) as f32 - 4.0) / 8.0).collect();
        let high: Vec<f32> = (0..192).map(|n| ((n % 13) as f32 - 6.0) / 16.0).collect();
        let get = |samples: &[f32], n: isize| if n < 0 { 0.0 } else { samples[n as usize] };
        let mut up = EmittedV1Interpolator::new(table(coefficients), placement(0, 0)).unwrap();
        for (block, samples) in low.chunks_exact(32).enumerate() {
            let actual = up
                .process_block(samples.try_into().unwrap(), INTERPOLATE_WORDS)
                .unwrap();
            for n in 0..32 {
                let time = (block * 32 + n) as isize;
                let mut phases = [0.0f32; 2];
                for j in 0..8 {
                    let recent = get(&low, time - j as isize);
                    let old = get(&low, time - 15 + j as isize);
                    phases[0] += coefficients[2 * j] * recent + coefficients[2 * j + 1] * old;
                    phases[1] += coefficients[2 * j] * old + coefficients[2 * j + 1] * recent;
                }
                assert_eq!(
                    actual.outputs[2 * n..2 * n + 2],
                    phases.map(|sum| 2.0 * sum)
                );
            }
        }
        let mut down = EmittedV1Decimator::new(table(coefficients), placement(0, 0)).unwrap();
        for (block, samples) in high.chunks_exact(64).enumerate() {
            let actual = down
                .process_block(samples.try_into().unwrap(), DECIMATE_WORDS)
                .unwrap();
            for (n, &output) in actual.outputs.iter().enumerate() {
                let time = (block * 64 + 2 * n) as isize;
                let expected: f32 = (0..16)
                    .map(|j| {
                        coefficients[j]
                            * (get(&high, time - j as isize) + get(&high, time - 31 + j as isize))
                    })
                    .sum();
                assert_eq!(output, expected);
            }
        }
        assert_matches_reference(coefficients, &low, &high);

        // Declared T0 asymmetry: phase 0 sees 2*x[n], phase 1 sees 2*x[n-15];
        // the return sees u[2n]+u[2n-31], never the odd sample u[2n+1].
        let mut t0 = [0.0; 16];
        t0[0] = 1.0;
        let mut up = EmittedV1Interpolator::new(table(t0), placement(0, 0)).unwrap();
        let mut impulse = [0.0; 32];
        impulse[0] = 1.0;
        let outputs = up
            .process_block(&impulse, INTERPOLATE_WORDS)
            .unwrap()
            .outputs;
        let mut expected = [0.0; 64];
        expected[0] = 2.0;
        expected[31] = 2.0;
        assert_eq!(outputs, expected);
        let mut down = EmittedV1Decimator::new(table(t0), placement(0, 0)).unwrap();
        let mut high = [0.0; 64];
        (high[0], high[1], high[33]) = (5.0, 7.0, 11.0);
        let mut expected = [0.0; 32];
        (expected[0], expected[16]) = (5.0, 7.0);
        assert_eq!(
            down.process_block(&high, DECIMATE_WORDS).unwrap().outputs,
            expected
        );
        let mut expected = [0.0; 32];
        expected[0] = 11.0; // u[64]+u[33] across the callback boundary.
        assert_eq!(
            down.process_block(&[0.0; 64], DECIMATE_WORDS)
                .unwrap()
                .outputs,
            expected
        );

        // Lane reduction: [2^24, 1, -2^24] on ones gives 2; one accumulator gives 0.
        let mut lanes = [0.0; 16];
        lanes[..3].copy_from_slice(&[16_777_216.0, 1.0, -16_777_216.0]);
        let mut down = EmittedV1Decimator::new(table(lanes), placement(0, 0)).unwrap();
        down.process_block(&[1.0; 64], DECIMATE_WORDS).unwrap();
        assert_eq!(
            down.process_block(&[1.0; 64], DECIMATE_WORDS)
                .unwrap()
                .outputs,
            [2.0; 32]
        );

        // Signed zeros and finite subnormals follow host f32, as the reference does.
        // All-negative taps make every product -0.0 on zero input: a lane or phase
        // that skipped the `0.0 + x` start would publish -0.0 instead of +0.0.
        let negative = std::array::from_fn(|j| -((j + 1) as f32) / 32.0);
        let tiny = f32::from_bits(1);
        let low: Vec<f32> = (0..64)
            .map(|n| match n % 4 {
                0 => -0.0,
                1 => tiny,
                2 => f32::MIN_POSITIVE,
                _ => -tiny * 3.0,
            })
            .collect();
        let high: Vec<f32> = (0..128).map(|n| low[n / 2]).collect();
        assert_matches_reference(negative, &low, &high);
        let mut down = EmittedV1Decimator::new(table(negative), placement(0, 0)).unwrap();
        let block = down.process_block(&[-0.0; 64], DECIMATE_WORDS).unwrap();
        assert_eq!(bits(block.outputs), [0; 32]);
        assert_eq!(bits(down.history()), [(-0.0f32).to_bits(); 31]);
    }

    #[test]
    fn raw_kernels_bind_placement_preserve_registers_and_refuse_atomically() {
        let t0 = {
            let mut coefficients = [0.0; 16];
            coefficients[0] = 1.0;
            table(coefficients)
        };
        let at = DmWordAddress32::new;
        assert_eq!(
            Layout::new(Kernel::Decimate, at(0), at(63), at(200)),
            Err(Error::Overlap {
                first: 0,
                second: 1
            })
        );
        // Topmost DM: history ends exactly at word u32::MAX; one word higher is refused.
        let top = u32::MAX - 15 + 1;
        let layout = Layout::new(Kernel::Interpolate, at(top - 96), at(top - 64), at(top)).unwrap();
        assert_eq!(
            Layout::new(Kernel::Interpolate, at(0), at(32), at(top + 1)),
            Err(Error::Address(AddressError::DmOverflow))
        );
        // Topmost PM: the RTS is word 0xffffff; one word higher is refused.
        let origin = PmWordAddress24::new(0x100_0000 - INTERPOLATE_WORDS as u32).unwrap();
        let compiled = CompiledResampling::compile(origin, layout, &t0).unwrap();
        assert_eq!(compiled.max_steps(), INTERPOLATE_WORDS);
        assert_eq!(
            compiled.coefficient_bits(),
            t0.coefficients().map(f32::to_bits)
        );
        let higher = PmWordAddress24::new(origin.get() + 1).unwrap();
        assert!(matches!(
            CompiledResampling::compile(higher, layout, &t0),
            Err(Error::Emit(EmitError::ProgramBounds { .. }))
        ));
        let decimate = Layout::new(Kernel::Decimate, at(0), at(64), at(96)).unwrap();
        assert_eq!(
            CompiledResampling::compile(PmWordAddress24::new(0).unwrap(), decimate, &t0)
                .unwrap()
                .max_steps(),
            DECIMATE_WORDS
        );

        let mut words: Vec<u32> = (0..111).map(|n| (n as f32 / 4.0).to_bits()).collect();
        words[96] = 3.0f32.to_bits(); // history[0] = x[-1]
        let memory = DmMemory::new(at(top - 96), words).unwrap();
        let state = ScalarState {
            data: std::array::from_fn(|r| 0xa5a5_0000 | r as u32),
            indices: [1, 2, 3, 4, 5, 6, 7, 8],
            modifiers: [-1; 8],
            bases: [9; 8],
            lengths: [10; 8],
            ..ScalarState::default()
        };

        let refuse = |state: &ScalarState, memory: &DmMemory, budget: usize| {
            let (mut s, mut m) = (state.clone(), memory.clone());
            let result = compiled.simulate_atomic(&mut s, &mut m, budget);
            assert_eq!((&s, &m), (state, memory), "refusal must not publish");
            result.unwrap_err()
        };
        // Late budget failure: every store and history copy ran, only RTS remained.
        assert_eq!(
            refuse(&state, &memory, INTERPOLATE_WORDS - 1),
            Error::Simulation(SimulationError::StepBudget(INTERPOLATE_WORDS - 1))
        );
        let mut bad_mode = state.clone();
        bad_mode.mode1 = 0;
        assert_eq!(refuse(&bad_mode, &memory, usize::MAX), Error::Modes(0));
        let short = DmMemory::new(at(top - 96), vec![0; 110]).unwrap();
        assert!(matches!(
            refuse(&state, &short, usize::MAX),
            Error::Simulation(SimulationError::MemoryBounds { .. })
        ));
        let mut nonfinite = memory.clone();
        nonfinite.write(at(u32::MAX), f32::NAN.to_bits()).unwrap();
        assert_eq!(
            refuse(&state, &nonfinite, usize::MAX),
            Error::NonFiniteMemory {
                address: at(u32::MAX)
            }
        );

        let (mut s, mut m) = (state.clone(), memory.clone());
        let receipt = compiled
            .simulate_atomic(&mut s, &mut m, INTERPOLATE_WORDS)
            .unwrap();
        assert_eq!((receipt.steps, receipt.stores), (INTERPOLATE_WORDS, 79));
        assert_eq!(m.writes(), compiled.publications());
        assert_eq!(s.data[9..], state.data[9..]);
        assert_eq!(
            (s.indices, s.modifiers, s.bases, s.lengths, s.mode1),
            (
                state.indices,
                state.modifiers,
                state.bases,
                state.lengths,
                state.mode1
            )
        );
        let outputs = &m.words()[32..96];
        // Phase 0 at n=1 is 2*x[1]; phase 1 at n=14 is 2*x[-1] from history.
        assert_eq!(
            (outputs[2], outputs[29]),
            (0.5f32.to_bits(), 6.0f32.to_bits())
        );
        assert_eq!(m.words()[96], (31.0f32 / 4.0).to_bits()); // history[0] = x[31]

        // Pair-before-product: u+u overflows, then the zero tap gives NaN, so the
        // block refuses although every half-product would have been finite.
        let zero = Layout::new(Kernel::Decimate, at(0), at(64), at(96)).unwrap();
        let zero =
            CompiledResampling::compile(PmWordAddress24::new(0).unwrap(), zero, &table([0.0; 16]))
                .unwrap();
        let mut words = vec![f32::MAX.to_bits(); 64];
        words.extend([0; 32 + 31]);
        let memory = DmMemory::new(at(0), words).unwrap();
        let (mut s, mut m) = (ScalarState::default(), memory.clone());
        assert!(matches!(
            zero.simulate_atomic(&mut s, &mut m, usize::MAX),
            Err(Error::NonFiniteMemory { .. })
        ));
        assert_eq!((s, m), (ScalarState::default(), memory));
    }

    #[test]
    fn wrapper_budget_retry_reset_reload_and_instance_isolation() {
        let first: [f32; 16] = std::array::from_fn(|j| (16 - j) as f32 / 128.0);
        let second: [f32; 16] = std::array::from_fn(|j| j as f32 / 256.0 - 0.03125);
        let input: [f32; 32] = std::array::from_fn(|n| (n as f32 - 13.0) / 4.0);
        let place = placement(0x40, 0x100);
        let mut up = EmittedV1Interpolator::new(table(first), place).unwrap();
        let mut twin = up.clone();
        up.process_block(&input, INTERPOLATE_WORDS).unwrap();
        twin.process_block(&input, INTERPOLATE_WORDS).unwrap();

        // A late budget failure and an arithmetic overflow both retain everything;
        // the retry is bitwise the run of an instance that never failed.
        let before = up.clone();
        assert_eq!(
            up.process_block(&input, INTERPOLATE_WORDS - 1),
            Err(Error::Simulation(SimulationError::StepBudget(
                INTERPOLATE_WORDS - 1
            )))
        );
        assert_eq!(up, before);
        let mut overflow = input;
        // Phase 0 at n=31 sums T[0,2..14] (0.5625) of MAX: finite; doubling is not.
        overflow[24..].fill(f32::MAX);
        assert!(matches!(
            up.process_block(&overflow, INTERPOLATE_WORDS),
            Err(Error::NonFiniteMemory { .. })
        ));
        assert_eq!(up, before);
        let retry = up.process_block(&input, INTERPOLATE_WORDS).unwrap();
        assert_eq!(
            retry,
            twin.process_block(&input, INTERPOLATE_WORDS).unwrap()
        );
        assert_eq!(up, twin);

        // Two instances with identical logical placement are independent.
        let mut other = EmittedV1Interpolator::new(table(first), place).unwrap();
        let mut reference = V1ResamplingReference::new(table(first));
        let other_input = [0.5; 32];
        assert_eq!(
            other
                .process_block(&other_input, INTERPOLATE_WORDS)
                .unwrap()
                .outputs,
            reference.interpolate_block(&other_input).unwrap()
        );
        assert_eq!(up, twin);

        // Reset keeps the image; reload recompiles with the reference history policy.
        let image = up.compiled().clone();
        let history = up.history();
        up.reset();
        assert_eq!((up.history(), up.compiled()), ([0.0; 15], &image));
        up.process_block(&input, INTERPOLATE_WORDS).unwrap();
        let mut reference = V1ResamplingReference::new(table(first));
        reference.interpolate_block(&input).unwrap();
        up.reload(table(second), HistoryUpdate::Preserve).unwrap();
        reference.reload(table(second), HistoryUpdate::Preserve);
        assert_eq!(up.history(), history);
        assert_ne!(up.compiled().code(), image.code());
        assert_eq!(up.compiled().layout(), image.layout());
        assert_eq!(up.compiled().code().origin(), image.code().origin());
        assert_eq!(up.table(), &table(second));
        assert_eq!(
            bits(up.process_block(&input, INTERPOLATE_WORDS).unwrap().outputs),
            bits(reference.interpolate_block(&input).unwrap())
        );
        up.reload(table(first), HistoryUpdate::Reset).unwrap();
        assert_eq!(up.compiled(), &image);
        assert_eq!(up.history(), [0.0; 15]);
    }
}

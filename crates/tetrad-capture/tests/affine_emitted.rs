use tetrad_capture::affine::emitted::{
    ABI, CODE_WORDS, CompiledAffine, Error, MAX_INPUTS, SAMPLES, compile_affine, simulate_affine,
};
use tetrad_capture::affine::{AffineRecord, Limits};
use tetrad_sharc::{
    address::{DmWordAddress32 as Dm, PmWordAddress24 as Pm},
    simulator::{DmMemory, ScalarState, SimulationError},
};

fn record(weights: &[f32], bias: f32, outputs: usize) -> Vec<u8> {
    let n = weights.len();
    let mut bytes = vec![0xab; 84 + 4 * n * outputs + 4 * outputs];
    for (offset, value) in [
        (0, 4),
        (4, 4),
        (32, n as u32),
        (44, outputs as u32),
        (60, 2),
    ] {
        bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }
    for (word, value) in bytes[76..76 + 4 * n].chunks_exact_mut(4).zip(weights) {
        word.copy_from_slice(&value.to_le_bytes());
    }
    bytes[84 + 4 * n * outputs..88 + 4 * n * outputs].copy_from_slice(&bias.to_le_bytes());
    bytes
}

fn compile(bytes: &[u8], origin: u32) -> CompiledAffine {
    let (parsed, _) = AffineRecord::parse(bytes, Limits::default()).unwrap();
    let n = parsed.dimensions().0 as u32;
    compile_affine(
        Pm::new(origin).unwrap(),
        &parsed,
        Dm::new(10),
        Dm::new(10 + 32 * n),
        Dm::new(10 + 33 * n),
        Dm::new(11 + 33 * n),
    )
    .unwrap()
}

fn setup(compiled: &CompiledAffine, inputs: &[f32]) -> (ScalarState, DmMemory) {
    let layout = compiled.layout();
    let mut words = vec![0xdead_beef; (layout.output().get() + SAMPLES + 4) as usize];
    for (word, input) in words[layout.input().get() as usize..]
        .iter_mut()
        .zip(inputs)
    {
        *word = input.to_bits();
    }
    words[layout.weights().get() as usize..(layout.weights().get() + layout.inputs()) as usize]
        .copy_from_slice(compiled.weight_bits());
    words[layout.bias().get() as usize] = compiled.bias_bits();
    let state = ScalarState {
        mode1: ABI.required_mode1_set,
        data: [0x1234; 16],
        ..ScalarState::default()
    };
    (state, DmMemory::new(Dm::new(0), words).unwrap())
}

#[test]
fn emitted_code_matches_ascending_separate_arithmetic_for_every_sample() {
    for n in [4, 8, 12] {
        let weights: Vec<_> = (0..n).map(|i| (i as f32 - 3.0) * 0.125).collect();
        let bytes = record(&weights, 1.25, 1);
        let (parsed, _) = AffineRecord::parse(&bytes, Limits::default()).unwrap();
        let compiled = compile(&bytes, 0x1000);
        let inputs: Vec<_> = (0..n * 32)
            .map(|i| (i as f32 % 17.0 - 8.0) * 0.25)
            .collect();
        let expected = parsed.reference_32(&inputs).unwrap().map(f32::to_bits);
        let (mut state, mut memory) = setup(&compiled, &inputs);
        let before = memory.clone();
        let outcome = simulate_affine(
            &compiled,
            &mut state,
            &mut memory,
            compiled.executed_steps(),
        )
        .unwrap();
        assert_eq!(outcome.steps, compiled.executed_steps());
        assert_eq!(outcome.stores, 32);
        let start = compiled.layout().output().get() as usize;
        assert_eq!(&memory.words()[start..start + 32], expected);
        assert_eq!(&memory.words()[..start], &before.words()[..start]);
        assert_eq!(&memory.words()[start + 32..], &before.words()[start + 32..]);
        assert_eq!(
            memory.writes().iter().map(|a| a.get()).collect::<Vec<_>>(),
            (start as u32..start as u32 + 32).collect::<Vec<_>>()
        );
        assert_eq!(state.data[2], 0x1234);
        assert_eq!(state.data[9], 0);
        assert_eq!(
            state.indices[0],
            compiled.layout().input().get() + 32 * n as u32
        );
        assert_eq!(
            state.indices[1],
            compiled.layout().weights().get() + n as u32
        );
    }
    let bytes = record(&[f32::from_bits(0x3f80_0001), 0.0, 0.0, 0.0], -1.0, 1);
    let compiled = compile(&bytes, 0);
    let inputs = [f32::from_bits(0x3f7f_fffe), 0.0, 0.0, 0.0].repeat(32);
    let (mut state, mut memory) = setup(&compiled, &inputs);
    simulate_affine(
        &compiled,
        &mut state,
        &mut memory,
        compiled.executed_steps(),
    )
    .unwrap();
    let start = compiled.layout().output().get() as usize;
    assert_eq!(&memory.words()[start..start + 32], &[0; 32]);
    assert_ne!(
        f32::from_bits(0x3f80_0001)
            .mul_add(f32::from_bits(0x3f7f_fffe), -1.0)
            .to_bits(),
        0
    );
}

#[test]
fn fixed_machine_image_relocates_and_pm_boundary_is_exact() {
    let bytes = record(&[1.0; 4], 0.0, 1);
    for origin in [0, 0x1000, 0xff_ffff - (CODE_WORDS as u32 - 1)] {
        let compiled = compile(&bytes, origin);
        assert_eq!(compiled.code().word_count(), CODE_WORDS);
        assert_eq!(compiled.code().isa48_le_bytes().len(), CODE_WORDS * 6);
        assert_eq!(
            compiled
                .code()
                .symbols()
                .iter()
                .find(|(name, _)| name.name() == "affine.return")
                .unwrap()
                .1
                .get(),
            origin + 353
        );
        for sample in 0..32 {
            // Normal relative JUMP NE back five PC words; no delayed branch.
            assert_eq!(
                compiled.code().words()[10 + sample * 11].bits(),
                0x0720_00ff_fffb
            );
        }
    }
    let (parsed, _) = AffineRecord::parse(&bytes, Limits::default()).unwrap();
    assert!(matches!(
        compile_affine(
            Pm::new(0xff_ffff - 352).unwrap(),
            &parsed,
            Dm::new(0),
            Dm::new(128),
            Dm::new(132),
            Dm::new(133)
        ),
        Err(Error::Emit(_))
    ));
}

#[test]
fn shape_tensor_and_dm_range_refusals_precede_successful_compilation() {
    for (n, o) in [(3, 1), (6, 1), (4, 3)] {
        let bytes = record(&vec![1.0; n], 0.0, o);
        let (parsed, _) = AffineRecord::parse(&bytes, Limits::default()).unwrap();
        assert_eq!(
            compile_affine(
                Pm::new(0).unwrap(),
                &parsed,
                Dm::new(0),
                Dm::new(1000),
                Dm::new(2000),
                Dm::new(3000)
            ),
            Err(Error::UnsupportedShape)
        );
    }
    let bytes = record(&vec![0.0; MAX_INPUTS], 0.0, 1);
    assert_eq!(compile(&bytes, 0).layout().inputs(), MAX_INPUTS as u32);
    let bytes = record(&vec![0.0; MAX_INPUTS + 4], 0.0, 1);
    let (parsed, _) = AffineRecord::parse(
        &bytes,
        Limits {
            max_dimension: usize::MAX,
            ..Limits::default()
        },
    )
    .unwrap();
    assert_eq!(
        compile_affine(
            Pm::new(0).unwrap(),
            &parsed,
            Dm::new(0),
            Dm::new(1_000_000),
            Dm::new(2_000_000),
            Dm::new(3_000_000)
        ),
        Err(Error::UnsupportedShape)
    );
    for index in [0, 4] {
        let mut bytes = record(&[1.0; 4], 0.0, 1);
        let offset = if index == 4 { 100 } else { 76 };
        bytes[offset..offset + 4].copy_from_slice(&f32::NAN.to_bits().to_le_bytes());
        let (parsed, _) = AffineRecord::parse(&bytes, Limits::default()).unwrap();
        assert_eq!(
            compile_affine(
                Pm::new(0).unwrap(),
                &parsed,
                Dm::new(0),
                Dm::new(128),
                Dm::new(132),
                Dm::new(133)
            ),
            Err(Error::NonFiniteCoefficient(index))
        );
    }
    let bytes = record(&[1.0; 4], 0.0, 1);
    let (parsed, _) = AffineRecord::parse(&bytes, Limits::default()).unwrap();
    let base = [0, 128, 132, 133];
    for left in 0..4 {
        for right in left + 1..4 {
            let mut overlap = base;
            overlap[right] = overlap[left];
            assert_eq!(
                compile_affine(
                    Pm::new(0).unwrap(),
                    &parsed,
                    Dm::new(overlap[0]),
                    Dm::new(overlap[1]),
                    Dm::new(overlap[2]),
                    Dm::new(overlap[3])
                ),
                Err(Error::DmOverlap)
            );
        }
    }
    for index in 0..4 {
        let mut overflow = base;
        overflow[index] = u32::MAX;
        assert!(matches!(
            compile_affine(
                Pm::new(0).unwrap(),
                &parsed,
                Dm::new(overflow[0]),
                Dm::new(overflow[1]),
                Dm::new(overflow[2]),
                Dm::new(overflow[3])
            ),
            Err(Error::Address(_))
        ));
    }
}

#[test]
fn simulation_refusals_are_atomic_and_preserve_prior_write_history() {
    let bytes = record(&[1.0; 4], 0.0, 1);
    let compiled = compile(&bytes, 0);
    for case in 0..7 {
        let (mut state, mut memory) = setup(&compiled, &[1.0; 128]);
        memory.write(Dm::new(1), 0x8888).unwrap();
        match case {
            0 => state.mode1 = 0,
            1 => state.lengths[1] = 8,
            2 => memory.write(compiled.layout().weights(), 0).unwrap(),
            3 => memory.write(compiled.layout().bias(), 1).unwrap(),
            4 => memory
                .write(compiled.layout().input(), f32::INFINITY.to_bits())
                .unwrap(),
            5 => memory = DmMemory::new(Dm::new(0), vec![0; 8]).unwrap(),
            _ => (),
        }
        let state_before = state.clone();
        let memory_before = memory.clone();
        let budget = if case == 6 {
            compiled.executed_steps() - 1
        } else {
            compiled.executed_steps()
        };
        let error = simulate_affine(&compiled, &mut state, &mut memory, budget).unwrap_err();
        match case {
            0 => assert!(matches!(
                error,
                Error::Simulation(SimulationError::Modes(_))
            )),
            1 => assert!(matches!(
                error,
                Error::Simulation(SimulationError::CircularLength { index: 1, .. })
            )),
            2 | 3 => assert!(matches!(error, Error::CoefficientMismatch(_))),
            4 => assert!(matches!(error, Error::NonFiniteInput(_))),
            5 => assert!(matches!(
                error,
                Error::Simulation(SimulationError::MemoryBounds { .. })
            )),
            6 => assert_eq!(
                error,
                Error::Simulation(SimulationError::StepBudget(budget))
            ),
            _ => unreachable!(),
        }
        assert_eq!(state, state_before);
        assert_eq!(memory, memory_before);
    }
    let bytes = record(&[f32::MAX, 1.0, 1.0, 1.0], 0.0, 1);
    let compiled = compile(&bytes, 0);
    let (mut state, mut memory) = setup(&compiled, &[2.0; 128]);
    let before = (state.clone(), memory.clone());
    assert_eq!(
        simulate_affine(
            &compiled,
            &mut state,
            &mut memory,
            compiled.executed_steps()
        ),
        Err(Error::NumericOverflow(0))
    );
    assert_eq!((state, memory), before);
}

#[test]
fn compiled_tensor_is_owned_and_each_invocation_reloads_bias_and_coefficients() {
    let mut bytes = record(&[1.0, 2.0, 3.0, 4.0], 11.0, 1);
    let compiled = compile(&bytes, 0);
    bytes.fill(0);
    assert_eq!(
        compiled.weight_bits(),
        &[
            1.0_f32.to_bits(),
            2.0_f32.to_bits(),
            3.0_f32.to_bits(),
            4.0_f32.to_bits()
        ]
    );
    assert_eq!(compiled.bias_bits(), 11.0_f32.to_bits());
    let (mut state, mut memory) = setup(&compiled, &[5.0, 7.0, 0.0, 0.0].repeat(32));
    for _ in 0..2 {
        simulate_affine(
            &compiled,
            &mut state,
            &mut memory,
            compiled.executed_steps(),
        )
        .unwrap();
        let start = compiled.layout().output().get() as usize;
        assert_eq!(
            &memory.words()[start..start + 32],
            &[30.0_f32.to_bits(); 32]
        );
    }
    assert_eq!(memory.writes().len(), 64);
}

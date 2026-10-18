use tetrad_media::ir::{IrInputFormat, IrLimits, IrSamples, decode};
use tetrad_media::owned_convolution::{
    CODE_WORDS, Convolver, Error, HistoryUpdate, InitialHistory, MAX_SCRATCH_WORDS, SAMPLES,
    compile,
};
use tetrad_sharc::address::{AddressError, DmWordAddress32, PmWordAddress24};
use tetrad_sharc::emit::EmitError;
use tetrad_sharc::simulator::SimulationError;

fn ir(values: &[f32]) -> IrSamples {
    let bytes: Vec<_> = values
        .iter()
        .flat_map(|value| value.to_le_bytes())
        .collect();
    decode(
        &bytes,
        IrInputFormat::RawLeFloat32,
        IrLimits {
            max_asset_bytes: bytes.len(),
            max_chunks: 0,
        },
    )
    .unwrap()
}

fn direct(sequence: &[f32], coefficients: &[f32], history_count: usize) -> [u32; SAMPLES] {
    std::array::from_fn(|sample| {
        let current = history_count + sample;
        let mut sum = 0.0_f32;
        for tap in (0..coefficients.len()).rev() {
            let product = coefficients[tap] * sequence[current - tap];
            sum += product;
        }
        sum.to_bits()
    })
}

#[test]
fn emitted_words_preserve_causal_order_and_history_across_blocks() {
    for count in [1, 3, 1024] {
        let coefficients: Vec<_> = (0..count).map(|i| (i as f32 % 7.0 - 3.0) / 128.0).collect();
        let history: Vec<_> = (0..count - 1)
            .map(|i| (i as f32 % 13.0 - 6.0) / 16.0)
            .collect();
        let mut processor = Convolver::new(
            &ir(&coefficients),
            PmWordAddress24::new(0x14000).unwrap(),
            DmWordAddress32::new(0),
            InitialHistory::Samples(&history),
        )
        .unwrap();
        let mut expected_history = history;
        for block in 0..2 {
            let input = std::array::from_fn(|i| (i as f32 - 16.0 + block as f32) / 8.0);
            let mut window = expected_history.clone();
            window.extend(input);
            let expected = direct(&window, &coefficients, count - 1);
            let outcome = processor.process_block(&input, usize::MAX).unwrap();
            assert_eq!(outcome.samples.map(f32::to_bits), expected);
            assert_eq!(outcome.steps, 3 + 32 * (6 * count + 5));
            assert_eq!(outcome.stores, 32);
            expected_history = window[32..].to_vec();
            assert_eq!(processor.history(), expected_history);
        }
    }
    let coefficient = f32::from_bits(0x3f80_0001);
    let input = f32::from_bits(0x3f7f_fffe);
    let mut processor = Convolver::new(
        &ir(&[coefficient, -1.0]),
        PmWordAddress24::new(0).unwrap(),
        DmWordAddress32::new(0),
        InitialHistory::Samples(&[1.0]),
    )
    .unwrap();
    let output = processor.process_block(&[input; 32], usize::MAX).unwrap();
    assert_eq!(output.samples[0].to_bits(), 0);
    assert_ne!(
        coefficient.mul_add(input, -1.0).to_bits(),
        output.samples[0].to_bits()
    );
}

#[test]
fn fixed_machine_image_and_compact_dm_pm_boundaries_are_exact() {
    for count in [1, 3, 1024] {
        let samples = ir(&vec![0.0; count]);
        let compiled = compile(
            &samples,
            PmWordAddress24::new(0x14000).unwrap(),
            DmWordAddress32::new(0x1000),
        )
        .unwrap();
        let layout = compiled.layout();
        let n = count as u64;
        assert_eq!(layout.coefficients().get(), 0x1000 + count as u32 + 31);
        assert_eq!(layout.output().get(), 0x1000 + 2 * count as u32 + 31);
        assert_eq!(layout.scratch_words() as usize, 2 * count + 63);
        assert!(layout.scratch_words() as usize <= MAX_SCRATCH_WORDS);
        let words: Vec<_> = compiled
            .code()
            .words()
            .iter()
            .map(|word| word.bits())
            .collect();
        assert_eq!(words.len(), CODE_WORDS);
        assert_eq!(words[..2], [0x0f20_0000_0001, 0x0f21_ffff_ffff]);
        for sample in 0..32_u64 {
            let offset = 2 + sample as usize * 11;
            assert_eq!(
                words[offset..offset + 11],
                [
                    0x0f10_0000_0000 | (0x1000 + sample),
                    0x0f11_0000_0000 | (u64::from(layout.coefficients().get()) + n - 1),
                    0x0f01_0000_0000,
                    0x0f09_0000_0000 | n,
                    0x503e_0000_0000,
                    0x527e_0200_0000,
                    0x013e_0013_0004,
                    0x013e_0008_1110,
                    0x013e_0002_a990,
                    0x0720_00ff_fffb,
                    0x1101_0000_0000 | (u64::from(layout.output().get()) + sample),
                ]
            );
        }
        assert_eq!(words[354], 0x0a3e_0000_0000);
        let bytes = compiled.code().isa48_le_bytes();
        assert_eq!(bytes.len(), CODE_WORDS * 6);
        for (word, encoded) in words.iter().zip(bytes.chunks_exact(6)) {
            assert_eq!(encoded, &word.to_le_bytes()[..6]);
        }
        let last_origin = 0xff_ffff - 354;
        assert!(
            compile(
                &samples,
                PmWordAddress24::new(last_origin).unwrap(),
                DmWordAddress32::new(0)
            )
            .is_ok()
        );
        assert!(matches!(
            compile(
                &samples,
                PmWordAddress24::new(last_origin + 1).unwrap(),
                DmWordAddress32::new(0)
            ),
            Err(Error::Emit(EmitError::ProgramBounds { .. }))
        ));
        let last_input = u32::MAX - layout.scratch_words();
        assert!(
            compile(
                &samples,
                PmWordAddress24::new(0).unwrap(),
                DmWordAddress32::new(last_input)
            )
            .is_ok()
        );
        assert!(matches!(
            compile(
                &samples,
                PmWordAddress24::new(0).unwrap(),
                DmWordAddress32::new(last_input + 1)
            ),
            Err(Error::Address(AddressError::DmOverflow))
        ));
    }
}

#[test]
fn import_and_history_admission_reject_unsupported_values_without_normalization() {
    let make = |samples: &IrSamples, history: InitialHistory<'_>| {
        Convolver::new(
            samples,
            PmWordAddress24::new(0).unwrap(),
            DmWordAddress32::new(0),
            history,
        )
    };
    assert!(matches!(
        make(&ir(&[]), InitialHistory::Zero),
        Err(Error::Length(0))
    ));
    for value in [
        f32::INFINITY,
        f32::NEG_INFINITY,
        f32::from_bits(0x7fc0_4321),
    ] {
        assert!(matches!(
            make(&ir(&[0.0, value]), InitialHistory::Zero),
            Err(Error::NonFiniteCoefficient(1))
        ));
        assert!(matches!(
            make(&ir(&[1.0, 2.0]), InitialHistory::Samples(&[value])),
            Err(Error::NonFiniteHistory(0))
        ));
    }
    assert!(matches!(
        make(&ir(&[1.0, 2.0]), InitialHistory::Samples(&[])),
        Err(Error::HistoryLength {
            expected: 1,
            actual: 0
        })
    ));
    let processor = make(
        &ir(&[-0.0, f32::from_bits(1)]),
        InitialHistory::Samples(&[-0.0]),
    )
    .unwrap();
    assert_eq!(processor.compiled().coefficient_bits(), &[0x8000_0000, 1]);
    assert_eq!(processor.history()[0].to_bits(), 0x8000_0000);
}

#[test]
fn budget_input_and_late_numeric_failure_preserve_history_and_compiled_image() {
    let coefficients = ir(&[2.0, 0.5, 0.25]);
    let mut processor = Convolver::new(
        &coefficients,
        PmWordAddress24::new(0x4000).unwrap(),
        DmWordAddress32::new(0x1000),
        InitialHistory::Samples(&[3.0, 4.0]),
    )
    .unwrap();
    let before = processor.history().to_vec();
    let code = processor.compiled().code().clone();
    for budget in [0, processor.compiled().executed_steps() - 1] {
        assert!(
            matches!(processor.process_block(&[1.0; 32], budget), Err(Error::Simulation(SimulationError::StepBudget(limit))) if limit == budget)
        );
        assert_eq!(processor.history(), before);
        assert_eq!(processor.compiled().code(), &code);
    }
    let mut input = [1.0; 32];
    input[31] = f32::NAN;
    assert!(matches!(
        processor.process_block(&input, usize::MAX),
        Err(Error::NonFiniteInput(31))
    ));
    input[31] = f32::MAX;
    assert!(matches!(
        processor.process_block(&input, usize::MAX),
        Err(Error::NumericOverflow(31))
    ));
    assert_eq!(processor.history(), before);
    assert_eq!(processor.compiled().code(), &code);
    let valid = [1.0; 32];
    let mut window = before;
    window.extend(valid);
    assert_eq!(
        processor
            .process_block(&valid, usize::MAX)
            .unwrap()
            .samples
            .map(f32::to_bits),
        direct(&window, &[2.0, 0.5, 0.25], 2)
    );
}

#[test]
fn owned_coefficients_reload_and_explicit_reset_have_distinct_history_policies() {
    let original = ir(&[1.0, 0.5, 0.25]);
    let mut processor = Convolver::new(
        &original,
        PmWordAddress24::new(0x4000).unwrap(),
        DmWordAddress32::new(0),
        InitialHistory::Zero,
    )
    .unwrap();
    let bits = original.bits().to_vec();
    drop(original);
    assert_eq!(processor.compiled().coefficient_bits(), bits);
    processor.process_block(&[3.0; 32], usize::MAX).unwrap();
    assert_eq!(processor.history(), &[3.0, 3.0]);
    let code = processor.compiled().code().clone();
    assert_eq!(
        processor.reload(&ir(&[2.0]), HistoryUpdate::Preserve),
        Err(Error::PreserveLength)
    );
    assert!(matches!(
        processor.reload(&ir(&[f32::NAN]), HistoryUpdate::Reset),
        Err(Error::NonFiniteCoefficient(0))
    ));
    assert_eq!(processor.history(), &[3.0, 3.0]);
    assert_eq!(processor.compiled().code(), &code);
    processor
        .reload(&ir(&[2.0, 0.0, 0.0]), HistoryUpdate::Preserve)
        .unwrap();
    assert_eq!(processor.history(), &[3.0, 3.0]);
    assert_eq!(
        processor
            .process_block(&[1.0; 32], usize::MAX)
            .unwrap()
            .samples,
        [2.0; 32]
    );
    processor.reset();
    assert_eq!(processor.history(), &[0.0, 0.0]);
    processor.reload(&ir(&[0.0]), HistoryUpdate::Reset).unwrap();
    assert!(processor.history().is_empty());
    assert_eq!(
        processor
            .process_block(&[1.0; 32], usize::MAX)
            .unwrap()
            .samples,
        [0.0; 32]
    );
}

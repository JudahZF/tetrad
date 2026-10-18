//! Owned emitted V1 tone leaf against the host-f32 `V1ToneReference`.
use tetrad_capture::Error as CaptureError;
use tetrad_capture::tones::emitted::{
    CompiledTone, EmittedV1ToneReference, Error, Layout, ToneLogicalPlacement,
};
use tetrad_capture::tones::{
    V1Tone, V1ToneCoefficients, V1ToneReference, v1_tone_coefficients_reference,
    v1_tone_gain_reference,
};
use tetrad_sharc::address::{AddressError, DmWordAddress32, PmWordAddress24};
use tetrad_sharc::isa48::MODE1_TRUNCATE;
use tetrad_sharc::simulator::{DmMemory, ScalarState, SimulationError, SimulationOutcome};

const BASE: u32 = 0xffff_0000;
const STEPS: usize = 366;
const RECEIPT: SimulationOutcome = SimulationOutcome {
    steps: STEPS,
    stores: 36,
};

fn placement() -> ToneLogicalPlacement {
    ToneLogicalPlacement {
        origin: PmWordAddress24::new(0x1400).unwrap(),
        dm_base: DmWordAddress32::new(BASE),
    }
}

fn law(tone: V1Tone, normalized: f32) -> V1ToneCoefficients {
    v1_tone_coefficients_reference(tone, v1_tone_gain_reference(tone, normalized).unwrap()).unwrap()
}

fn coefficients(feedforward: [f32; 3], feedback: [f32; 2]) -> V1ToneCoefficients {
    V1ToneCoefficients {
        feedforward,
        feedback,
    }
}

fn mixed(block: usize) -> [f32; 32] {
    std::array::from_fn(|index| match (block * 32 + index) % 6 {
        0 => -0.0,
        1 => 1.0,
        2 => -0.75,
        _ => (index as f32 - 15.0) / 17.0,
    })
}

#[test]
fn one_leaf_matches_host_reference_bitwise_for_all_three_tones() {
    let image = EmittedV1ToneReference::new(law(V1Tone::First, 0.5), placement())
        .unwrap()
        .compiled()
        .clone();
    assert_eq!(image.max_steps(), STEPS);
    assert_eq!(image.code().isa48_le_bytes().len(), 2196);
    let words = image.code().words();
    // Coefficient b0 load into R4, first input load into R0, p0 = F4*F0, RTS.
    assert_eq!(words[0].bits(), 0x1004_0000_0000 | u64::from(BASE + 64));
    assert_eq!(words[9].bits(), 0x1000_0000_0000 | u64::from(BASE));
    assert_eq!(words[10].bits(), 0x013e_0013_0c40);
    assert_eq!(words[STEPS - 1].bits(), 0x0a3e_0000_0000);
    for tone in V1Tone::ALL {
        for normalized in [0.0, 0.37, 1.0] {
            let c = law(tone, normalized);
            let mut raw = EmittedV1ToneReference::new(c, placement()).unwrap();
            let mut host = V1ToneReference::new(c).unwrap();
            // Same code for every tone: the law lives only in coefficient words.
            assert_eq!(raw.compiled(), &image);
            for block in 0..3 {
                let inputs = mixed(block);
                let result = raw.process_block(&inputs, STEPS).unwrap();
                assert_eq!(result.receipt, RECEIPT);
                assert_eq!(
                    result.outputs.map(f32::to_bits),
                    host.process_block(&inputs).unwrap().map(f32::to_bits)
                );
                assert_eq!(
                    raw.history().map(f32::to_bits),
                    host.history().map(f32::to_bits)
                );
            }
        }
    }
}

#[test]
fn separate_f32_operation_order_is_not_fused_or_regrouped() {
    // ((x1*b1 + b0*x) + x2*b2): 2^24 + 1 rounds to 2^24 before -2^24 cancels it.
    // A fused or reassociated feedforward would give 1.0 at sample 2.
    let cancel = coefficients([1.0, 16_777_216.0, -16_777_216.0], [0.0; 2]);
    let mut raw = EmittedV1ToneReference::new(cancel, placement()).unwrap();
    assert_eq!(
        raw.process_block(&[1.0; 32], STEPS).unwrap().outputs[2].to_bits(),
        0
    );

    // Feedback is (s - q1) - q2: 1 + 2^-24 ties to 1.0 twice, whereas
    // s - (q1 + q2) would give 1 + 2^-23. History comes from an identity block.
    let mut raw =
        EmittedV1ToneReference::new(coefficients([1.0, 0.0, 0.0], [0.0; 2]), placement()).unwrap();
    raw.process_block(&[1.0; 32], STEPS).unwrap();
    assert_eq!(raw.history(), [1.0; 4]);
    let tiny = -f32::from_bits(0x3380_0000); // -2^-24
    raw.update_coefficients(coefficients([1.0, 0.0, 0.0], [tiny, tiny]))
        .unwrap();
    let outputs = raw.process_block(&[1.0; 32], STEPS).unwrap().outputs;
    assert!(outputs.iter().all(|y| y.to_bits() == 1.0f32.to_bits()));
}

#[test]
fn coefficient_update_preserves_history_and_reset_is_explicit() {
    let first = law(V1Tone::First, 0.9);
    let third = law(V1Tone::Third, 0.2);
    let mut raw = EmittedV1ToneReference::new(first, placement()).unwrap();
    let mut host = V1ToneReference::new(first).unwrap();
    raw.process_block(&mixed(0), STEPS).unwrap();
    host.process_block(&mixed(0)).unwrap();
    let history = raw.history();
    assert_ne!(history, [0.0; 4]);

    raw.update_coefficients(third).unwrap();
    host.update_coefficients(third).unwrap();
    assert_eq!(raw.history(), history);
    assert_eq!(raw.coefficients(), third);
    assert_eq!(
        raw.process_block(&mixed(1), STEPS)
            .unwrap()
            .outputs
            .map(f32::to_bits),
        host.process_block(&mixed(1)).unwrap().map(f32::to_bits)
    );

    let before = raw.clone();
    let mut bad = third;
    bad.feedback[1] = f32::INFINITY;
    assert_eq!(
        raw.update_coefficients(bad),
        Err(Error::NonFiniteCoefficient { slot: 4 })
    );
    assert_eq!(raw, before);

    raw.reset();
    assert_eq!(
        raw,
        EmittedV1ToneReference::new(third, placement()).unwrap()
    );
}

#[test]
fn budget_input_and_late_arithmetic_failures_retain_complete_instance() {
    let mut raw = EmittedV1ToneReference::new(law(V1Tone::Second, 0.8), placement()).unwrap();
    raw.process_block(&mixed(0), STEPS).unwrap();
    let before = raw.clone();
    for budget in [0, STEPS / 2, STEPS - 1] {
        assert_eq!(
            raw.process_block(&mixed(1), budget),
            Err(Error::Simulation(SimulationError::StepBudget(budget)))
        );
        assert_eq!(raw, before);
    }
    for invalid in [f32::NAN, f32::NEG_INFINITY] {
        let mut inputs = mixed(1);
        inputs[31] = invalid;
        assert_eq!(
            raw.process_block(&inputs, STEPS),
            Err(Error::NonFiniteMemory {
                address: DmWordAddress32::new(BASE + 31)
            })
        );
        assert_eq!(raw, before);
    }
    let expected = before.clone().process_block(&mixed(1), STEPS).unwrap();
    assert_eq!(raw.process_block(&mixed(1), STEPS).unwrap(), expected);

    // Only the last sample overflows: 31 outputs were already stored when it fails.
    let huge = coefficients([f32::MAX, 0.0, 0.0], [0.0; 2]);
    raw.update_coefficients(huge).unwrap();
    raw.reset();
    let mut host = V1ToneReference::new(huge).unwrap();
    let before = raw.clone();
    let mut inputs = [0.0; 32];
    inputs[31] = 2.0;
    assert_eq!(
        raw.process_block(&inputs, STEPS),
        Err(Error::NonFiniteMemory {
            address: DmWordAddress32::new(BASE + 32 + 31)
        })
    );
    assert_eq!(raw, before);
    assert_eq!(
        host.process_block(&inputs),
        Err(CaptureError::NumericOverflow)
    );
}

#[test]
fn placement_layout_store_sequence_and_staged_preconditions_are_checked() {
    let at = |origin: u32, dm_base: u32| ToneLogicalPlacement {
        origin: PmWordAddress24::new(origin).unwrap(),
        dm_base: DmWordAddress32::new(dm_base),
    };
    let c = law(V1Tone::Third, 0.6);
    // The last 366-word PM origin and the last 73-word representable DM end are admitted.
    assert!(EmittedV1ToneReference::new(c, at(0xff_ffff - 365, u32::MAX - 73)).is_ok());
    assert_eq!(
        EmittedV1ToneReference::new(c, at(0, u32::MAX - 72)),
        Err(Error::Address(AddressError::DmOverflow))
    );
    assert!(matches!(
        EmittedV1ToneReference::new(c, at(0xff_ffff - 364, 0)),
        Err(Error::Emit(_))
    ));
    let mut bad = c;
    bad.feedforward[0] = f32::NAN;
    assert_eq!(
        EmittedV1ToneReference::new(bad, placement()),
        Err(Error::NonFiniteCoefficient { slot: 0 })
    );

    let word = DmWordAddress32::new;
    assert_eq!(
        Layout::new(word(0), word(40), word(100), word(104)),
        Err(Error::Overlap {
            first: 2,
            second: 3
        })
    );
    // Non-adjacent caller layout: outputs, then x1, x2, y1, y2.
    let layout = Layout::new(word(0x200), word(0x100), word(0x10), word(0x20)).unwrap();
    let image = CompiledTone::compile(placement().origin, layout).unwrap();
    let expected: Vec<_> = (0..32)
        .map(|n| word(0x100 + n))
        .chain((0..4).map(|slot| word(0x20 + slot)))
        .collect();
    assert_eq!(image.publications(), expected);

    // Direct staging: DM covers 0x10..0x220. Coefficients identity, history zero.
    let mut words = vec![0u32; 0x210];
    words[0] = 1.0f32.to_bits();
    for n in 0..32 {
        words[0x1f0 + n] = (n as f32).to_bits();
    }
    let memory = DmMemory::new(word(0x10), words).unwrap();
    let mut state = ScalarState::default();
    let mut staged = memory.clone();
    assert_eq!(
        image.simulate_atomic(&mut state, &mut staged, STEPS),
        Ok(RECEIPT)
    );
    assert_eq!(staged.writes(), expected);
    assert_eq!(staged.read(word(0x100 + 7)), Ok(7.0f32.to_bits()));
    // DAG registers are untouched; only the data registers changed.
    assert_eq!(state.indices, ScalarState::default().indices);

    let mut modes = ScalarState {
        mode1: ScalarState::default().mode1 | MODE1_TRUNCATE,
        ..ScalarState::default()
    };
    let mut untouched = memory.clone();
    assert!(matches!(
        image.simulate_atomic(&mut modes, &mut untouched, STEPS),
        Err(Error::Modes(_))
    ));
    let mut nonfinite = memory.clone();
    nonfinite.write(word(0x13), f32::NAN.to_bits()).unwrap();
    let logged = nonfinite.clone();
    assert_eq!(
        image.simulate_atomic(&mut state.clone(), &mut nonfinite, STEPS),
        Err(Error::NonFiniteCoefficient { slot: 3 })
    );
    assert_eq!(nonfinite, logged);
    let mut short = DmMemory::new(word(0x10), vec![0; 0x20f]).unwrap();
    assert!(matches!(
        image.simulate_atomic(&mut ScalarState::default(), &mut short, STEPS),
        Err(Error::Simulation(SimulationError::MemoryBounds { .. }))
    ));
}

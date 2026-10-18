use tetrad_capture::Error;
use tetrad_capture::gain::GainAdapter;
use tetrad_capture::gain::emitted::{CompiledGainSmoother, Error as LeafError};
use tetrad_capture::reference::GainSmoother;
use tetrad_sharc::EmitError;
use tetrad_sharc::address::{AddressError, DmWordAddress32, PmWordAddress24};
use tetrad_sharc::simulator::{SimulationError, SimulationOutcome};

fn assert_unchanged(adapter: &GainAdapter, before: &GainAdapter) {
    assert_eq!(adapter.current().to_bits(), before.current().to_bits());
    assert_eq!(adapter.target().to_bits(), before.target().to_bits());
    assert_eq!(adapter.compiled(), before.compiled());
}

fn samples(seed: u32) -> [f32; 32] {
    let mut value = seed;
    std::array::from_fn(|index| {
        value = value.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        match index {
            0 => -0.0,
            1 => 0.0,
            2 => f32::from_bits(1),
            _ => f32::from_bits((value & 0x807f_ffff) | (((value >> 8) % 0xfe) << 23)),
        }
    })
}

#[test]
fn multiple_blocks_target_updates_and_instances_match_reference_bits() {
    let mut first = GainAdapter::new(
        PmWordAddress24::new(0x2000).unwrap(),
        DmWordAddress32::new(0x4000),
        0.25,
        2.0,
    )
    .unwrap();
    let mut second = GainAdapter::new(
        PmWordAddress24::new(0x3000).unwrap(),
        DmWordAddress32::new(0x5000),
        1.0,
        0.125,
    )
    .unwrap();
    let mut first_reference = GainSmoother::from_state(0.25, 2.0).unwrap();
    let mut second_reference = GainSmoother::from_state(1.0, 0.125).unwrap();
    let first_backend = first.compiled().clone();
    let second_backend = second.compiled().clone();
    for block in 0..5 {
        if block == 1 || block == 3 {
            let target = if block == 1 { 0.5 } else { 1.5 };
            let current = first.current().to_bits();
            first.set_linear_target(target).unwrap();
            first_reference.set_linear_target(target).unwrap();
            assert_eq!(first.current().to_bits(), current);
        }
        let first_inputs = samples(11 + block);
        let second_inputs = samples(71 + block);
        let mut expected_first = [0.0; 32];
        let mut expected_second = [0.0; 32];
        first_reference
            .process_chunk(&first_inputs, &mut expected_first)
            .unwrap();
        second_reference
            .process_chunk(&second_inputs, &mut expected_second)
            .unwrap();
        let second_before = second.clone();
        let result = first.process_block(&first_inputs, 230).unwrap();
        assert_unchanged(&second, &second_before);
        assert_eq!(
            result.outputs.map(f32::to_bits),
            expected_first.map(f32::to_bits)
        );
        assert_eq!(
            result.receipt,
            SimulationOutcome {
                steps: 230,
                stores: 33
            }
        );
        let first_before = first.clone();
        let result = second.process_block(&second_inputs, 230).unwrap();
        assert_unchanged(&first, &first_before);
        assert_eq!(
            result.outputs.map(f32::to_bits),
            expected_second.map(f32::to_bits)
        );
        assert_eq!(
            result.receipt,
            SimulationOutcome {
                steps: 230,
                stores: 33
            }
        );
        for (adapter, reference) in [(&first, &first_reference), (&second, &second_reference)] {
            assert_eq!(adapter.current().to_bits(), reference.current().to_bits());
            assert_eq!(adapter.target().to_bits(), reference.target().to_bits());
        }
        assert_eq!(first.compiled(), &first_backend);
        assert_eq!(second.compiled(), &second_backend);
    }
}

#[test]
fn rejected_controls_inputs_and_late_execution_keep_adapter_and_image_bits() {
    let mut adapter = GainAdapter::new(
        PmWordAddress24::new(0x2000).unwrap(),
        DmWordAddress32::new(0x4000),
        -0.0,
        2.0,
    )
    .unwrap();
    let before = adapter.clone();
    for value in [
        f32::from_bits(0x7fc0_1234),
        f32::INFINITY,
        f32::NEG_INFINITY,
        -1.0,
    ] {
        let expected = if value.is_finite() {
            Error::LinearGainRange
        } else {
            Error::NonFinite
        };
        assert_eq!(adapter.set_linear_target(value), Err(expected));
        assert_unchanged(&adapter, &before);
        assert!(adapter.reseed(value, 1.0).is_err());
        assert_unchanged(&adapter, &before);
        // Valid current must not commit before an invalid target is checked.
        assert!(adapter.reseed(3.0, value).is_err());
        assert_unchanged(&adapter, &before);
        assert!(
            GainAdapter::new(
                PmWordAddress24::new(0).unwrap(),
                DmWordAddress32::new(0),
                value,
                1.0
            )
            .is_err()
        );
        assert!(
            GainAdapter::new(
                PmWordAddress24::new(0).unwrap(),
                DmWordAddress32::new(0),
                1.0,
                value
            )
            .is_err()
        );
    }
    for index in 0..32 {
        for value in [
            f32::from_bits(0x7fc0_1234),
            f32::INFINITY,
            f32::NEG_INFINITY,
        ] {
            let mut inputs = [1.0; 32];
            inputs[index] = value;
            let input_bits = inputs.map(f32::to_bits);
            assert_eq!(
                adapter.process_block(&inputs, 230),
                Err(Error::Gain(LeafError::NonFiniteMemory {
                    address: DmWordAddress32::new(0x4000 + index as u32)
                }))
            );
            assert_eq!(inputs.map(f32::to_bits), input_bits);
            assert_unchanged(&adapter, &before);
        }
    }
    assert_eq!(
        adapter.process_block(&[1.0; 32], 229),
        Err(Error::Gain(LeafError::Simulation(
            SimulationError::StepBudget(229)
        )))
    );
    assert_unchanged(&adapter, &before);
    adapter.reseed(4.0, 4.0).unwrap();
    let before = adapter.clone();
    let mut inputs = [1.0; 32];
    inputs[31] = f32::MAX;
    let input_bits = inputs.map(f32::to_bits);
    // Whole caller-image/log/register rollback for these late cases is leaf coverage
    // in owned_gain_smoothing.rs; the adapter's per-call scratch DM is discarded.
    assert_eq!(
        adapter.process_block(&inputs, 230),
        Err(Error::Gain(LeafError::NonFiniteMemory {
            address: DmWordAddress32::new(0x403f)
        }))
    );
    assert_eq!(inputs.map(f32::to_bits), input_bits);
    assert_unchanged(&adapter, &before);
}

#[test]
fn validated_reseed_preserves_backend_top_placement_and_execution_receipt() {
    let pm = 0xff_ffff - 229;
    let base = u32::MAX - 66;
    let mut adapter = GainAdapter::new(
        PmWordAddress24::new(pm).unwrap(),
        DmWordAddress32::new(base),
        0.25,
        2.0,
    )
    .unwrap();
    let backend: CompiledGainSmoother = adapter.compiled().clone();
    let word_storage = adapter.compiled().code().words().as_ptr();
    assert_eq!(backend.code().origin().get(), pm);
    assert_eq!(backend.code().word_count(), 230);
    assert_eq!(backend.code().isa48_le_bytes().len(), 1380);
    assert_eq!(
        backend.layout().spans(),
        [
            (DmWordAddress32::new(base), 32),
            (DmWordAddress32::new(base + 32), 32),
            (DmWordAddress32::new(base + 64), 1),
            (DmWordAddress32::new(base + 65), 1),
        ]
    );
    for (current, target) in [
        (-0.0, -0.0),
        (f32::from_bits(1), f32::from_bits(3)),
        (0.5, 1.5),
    ] {
        adapter.reseed(current, target).unwrap();
        assert_eq!(
            (adapter.current().to_bits(), adapter.target().to_bits()),
            (current.to_bits(), target.to_bits())
        );
        assert_eq!(adapter.compiled(), &backend);
        assert_eq!(adapter.compiled().code().words().as_ptr(), word_storage);
        let mut reference = GainSmoother::from_state(current, target).unwrap();
        let inputs = samples(9);
        let mut expected = [0.0; 32];
        reference.process_chunk(&inputs, &mut expected).unwrap();
        let result = adapter.process_block(&inputs, usize::MAX).unwrap();
        assert_eq!(result.outputs.map(f32::to_bits), expected.map(f32::to_bits));
        assert_eq!(adapter.current().to_bits(), reference.current().to_bits());
        assert_eq!(adapter.target().to_bits(), target.to_bits());
        assert_eq!(
            result.receipt,
            SimulationOutcome {
                steps: 230,
                stores: 33
            }
        );
    }
    assert_eq!(adapter.compiled(), &backend);
    assert!(matches!(
        GainAdapter::new(
            PmWordAddress24::new(pm + 1).unwrap(),
            DmWordAddress32::new(base),
            0.0,
            1.0
        ),
        Err(Error::Gain(LeafError::Emit(
            EmitError::ProgramBounds { .. }
        )))
    ));
    assert!(matches!(
        GainAdapter::new(
            PmWordAddress24::new(pm).unwrap(),
            DmWordAddress32::new(base + 1),
            0.0,
            1.0
        ),
        Err(Error::Gain(LeafError::Address(AddressError::DmOverflow)))
    ));
}

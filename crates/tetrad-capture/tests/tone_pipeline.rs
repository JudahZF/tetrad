//! Owned emitted V1 tone stack against the host-f32 `V1ToneStackReference`.
use tetrad_capture::Error;
use tetrad_capture::tones::emitted::{Error as LeafError, ToneLogicalPlacement};
use tetrad_capture::tones::emitted_stack::{EmittedV1ToneStackReference, V1ToneInstructionBudgets};
use tetrad_capture::tones::{V1Tone, V1ToneStackReference};
use tetrad_sharc::address::{AddressError, DmWordAddress32, PmWordAddress24};
use tetrad_sharc::simulator::{SimulationError, SimulationOutcome};

const FULL: V1ToneInstructionBudgets = V1ToneInstructionBudgets {
    first: 366,
    second: 366,
    third: 366,
};
const RECEIPT: SimulationOutcome = SimulationOutcome {
    steps: 366,
    stores: 36,
};

fn at(origin: u32, dm_base: u32) -> ToneLogicalPlacement {
    ToneLogicalPlacement {
        origin: PmWordAddress24::new(origin).unwrap(),
        dm_base: DmWordAddress32::new(dm_base),
    }
}

/// Distinct placements and one placement shared by all three private invocations.
fn placements() -> [[ToneLogicalPlacement; 3]; 2] {
    [
        [
            at(0x1400, 0x1000),
            at(0x1800, 0x2000),
            at(0x1c00, 0xffff_0000),
        ],
        [at(0x1400, 0x1000); 3],
    ]
}

fn mixed(block: usize) -> [f32; 32] {
    std::array::from_fn(|index| match (block * 32 + index) % 5 {
        0 => -0.0,
        1 => 0.875,
        _ => ((index * 7 + block * 3) % 19) as f32 / 16.0 - 0.5,
    })
}

fn histories(stack: &EmittedV1ToneStackReference) -> [[u32; 4]; 3] {
    V1Tone::ALL.map(|tone| stack.section(tone).history().map(f32::to_bits))
}

fn host_histories(stack: &V1ToneStackReference) -> [[u32; 4]; 3] {
    V1Tone::ALL.map(|tone| stack.section(tone).history().map(f32::to_bits))
}

#[test]
fn emitted_stack_matches_host_stack_bitwise_across_blocks_updates_and_reset() {
    for placements in placements() {
        let normalized = [0.9, 0.2, 0.75];
        let mut raw = EmittedV1ToneStackReference::new(normalized, placements).unwrap();
        let mut host = V1ToneStackReference::from_normalized(normalized).unwrap();
        assert_eq!(raw.full_budgets(), FULL);
        let images = V1Tone::ALL.map(|tone| raw.section(tone).compiled().clone());
        for (image, placement) in images.iter().zip(placements) {
            assert_eq!(image.code().origin(), placement.origin);
            assert_eq!(image.layout().spans()[0].0, placement.dm_base);
        }
        for block in 0..5 {
            if block == 2 {
                // Block-boundary update: coefficients change, histories and code stay.
                let kept = histories(&raw);
                raw.set_normalized([0.1, 1.0, -0.0]).unwrap();
                host.set_normalized([0.1, 1.0, -0.0]).unwrap();
                assert_eq!(histories(&raw), kept);
                assert_eq!(raw.normalized()[2].to_bits(), (-0.0f32).to_bits());
            }
            if block == 4 {
                raw.reset();
                host.reset();
                assert_eq!(histories(&raw), [[0; 4]; 3]);
                assert_eq!(raw.normalized()[0], 0.1);
            }
            let inputs = mixed(block);
            let result = raw.process_block(&inputs, FULL).unwrap();
            let expected = host.process_block(&inputs).unwrap();
            assert_ne!(expected, inputs);
            assert_eq!(result.outputs.map(f32::to_bits), expected.map(f32::to_bits));
            assert_eq!(result.receipts, [RECEIPT; 3]);
            assert_eq!(histories(&raw), host_histories(&host));
            for tone in V1Tone::ALL {
                assert_eq!(
                    raw.section(tone).coefficients(),
                    host.section(tone).coefficients()
                );
                assert_eq!(raw.section(tone).compiled(), &images[tone as usize]);
            }
        }
    }
}

#[test]
fn late_leaf_and_input_failures_retain_all_three_sections_then_retry_matches() {
    let [placements, _] = placements();
    let mut raw = EmittedV1ToneStackReference::new([0.9, 0.2, 0.75], placements).unwrap();
    raw.process_block(&mixed(0), FULL).unwrap();
    let before = raw.clone();
    let step_budget = |budget| {
        Err(Error::Tone(LeafError::Simulation(
            SimulationError::StepBudget(budget),
        )))
    };
    // First, second and, after two leaves have advanced on the stage, third.
    for (budgets, budget) in [
        (V1ToneInstructionBudgets { first: 0, ..FULL }, 0),
        (
            V1ToneInstructionBudgets {
                second: 365,
                ..FULL
            },
            365,
        ),
        (V1ToneInstructionBudgets { third: 365, ..FULL }, 365),
    ] {
        assert_eq!(raw.process_block(&mixed(1), budgets), step_budget(budget));
        assert_eq!(raw, before);
    }
    let mut invalid = mixed(1);
    invalid[31] = f32::NAN;
    assert!(matches!(
        raw.process_block(&invalid, FULL),
        Err(Error::Tone(LeafError::NonFiniteMemory { .. }))
    ));
    assert_eq!(raw, before);
    let expected = before.clone().process_block(&mixed(1), FULL).unwrap();
    assert_eq!(raw.process_block(&mixed(1), FULL).unwrap(), expected);
}

#[test]
fn construction_and_controls_validate_all_three_before_committing() {
    let [placements, _] = placements();
    for (normalized, error) in [
        ([0.5, 0.5, f32::NAN], Error::NonFinite),
        ([0.5, 0.5, 1.5], Error::NormalizedRange),
        ([0.5, -0.25, 0.5], Error::NormalizedRange),
    ] {
        assert_eq!(
            EmittedV1ToneStackReference::new(normalized, placements).unwrap_err(),
            error
        );
        let mut raw = EmittedV1ToneStackReference::new([0.5; 3], placements).unwrap();
        raw.process_block(&mixed(0), FULL).unwrap();
        let before = raw.clone();
        assert_eq!(raw.set_normalized(normalized), Err(error));
        assert_eq!(raw, before);
    }
    // Only the third placement is invalid; no default placement substitutes.
    let [first, second, _] = placements;
    assert_eq!(
        EmittedV1ToneStackReference::new([0.5; 3], [first, second, at(0, u32::MAX - 72)]),
        Err(Error::Tone(LeafError::Address(AddressError::DmOverflow)))
    );
    assert!(matches!(
        EmittedV1ToneStackReference::new([0.5; 3], [first, second, at(0xff_ffff - 364, 0)]),
        Err(Error::Tone(LeafError::Emit(_)))
    ));
}

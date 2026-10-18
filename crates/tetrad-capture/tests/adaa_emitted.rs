//! Owned `adaa_clip_block32` leaf: raw ISA48 words under the normal-only runner
//! against an independent std-f32 reference of the owned contract (§4.1/§4.2).

use tetrad_capture::network::emitted::{
    CompiledAdaaClip, Error, Layout, OwnedAdaaClip, Preactivation, Receipt, SAMPLES, StepPlan,
};
use tetrad_capture::reference::{ADAA_EPSILON, AdaaClip};
use tetrad_sharc::address::{AddressError, DmWordAddress32, PmWordAddress24};
use tetrad_sharc::emit::EmitError;
use tetrad_sharc::simulator::{DmMemory, ScalarState, SimulationError};

const N: usize = SAMPLES as usize;
const BUDGET: usize = 10_000;

/// Written from the contract with std f32 ops, not from the emitter or host model.
/// Returns the output and whether each sample took the quotient path.
fn reference(previous: f32, input: &[f32]) -> (Vec<f32>, Vec<bool>) {
    let antiderivative = |v: f32| {
        let b = v.abs().min(2.0);
        (((f32::from_bits(0xbdaa_aaab) * b + 0.5) * b - 1.0) * b) + v.abs()
    };
    let mut p = previous;
    input
        .iter()
        .map(|&x| {
            let d = x - p;
            let m = d.abs().max(ADAA_EPSILON);
            let quotient = m != ADAA_EPSILON;
            let y = if quotient {
                let mut r = f32::from_bits(0x7ef3_11c7 - m.to_bits());
                for _ in 0..7 {
                    r *= 2.0 - m * r;
                }
                ((antiderivative(x) - antiderivative(p)) * 1.0f32.copysign(d)) * r
            } else {
                let c = ((x + p) * 0.5).clamp(-2.0, 2.0);
                c * (1.0 - c.abs() * 0.25)
            };
            p = x;
            (y, quotient)
        })
        .unzip()
}

fn dm(address: u32) -> DmWordAddress32 {
    DmWordAddress32::new(address)
}

fn origin() -> PmWordAddress24 {
    PmWordAddress24::new(0x1000).unwrap()
}

/// Input 0x100, output 0x140, state 0x180 in a 0x100..0x190 region.
fn fixture(state_value: f32, input: &[f32]) -> (CompiledAdaaClip, DmMemory) {
    let layout = Layout::new(dm(0x100), dm(0x140), dm(0x180)).unwrap();
    let mut words = vec![0xdead_beef; 0x90];
    for (word, value) in words.iter_mut().zip(input) {
        *word = value.to_bits();
    }
    words[0x80] = state_value.to_bits();
    let memory = DmMemory::new(dm(0x100), words).unwrap();
    (CompiledAdaaClip::compile(origin(), layout).unwrap(), memory)
}

fn bits(memory: &DmMemory, start: u32, words: u32) -> Vec<u32> {
    (start..start + words)
        .map(|address| memory.read(dm(address)).unwrap())
        .collect()
}

fn host_block(model: &mut OwnedAdaaClip, input: &[f32]) -> Result<Vec<f32>, Error> {
    let block: [Preactivation; N] = std::array::from_fn(|i| Preactivation::new(input[i]).unwrap());
    model
        .process_block(&block)
        .map(|block| block.output.to_vec())
}

fn to_bits(values: &[f32]) -> Vec<u32> {
    values.iter().map(|value| value.to_bits()).collect()
}

#[test]
fn public_step_plan_checks_counts_and_field_arithmetic_in_both_profiles() {
    let (compiled, _) = fixture(0.0, &[0.0; N]);
    let plan = compiled.step_plan();
    for count in 0..=SAMPLES {
        assert_eq!(plan.steps(count), Some(916 + 20 * count as usize));
    }
    let plans = [
        plan,
        StepPlan {
            fixed: usize::MAX,
            common: 1,
            reciprocal: 0,
            midpoint: 0,
        },
        StepPlan {
            common: usize::MAX,
            ..plan
        },
        StepPlan {
            reciprocal: usize::MAX,
            ..plan
        },
        StepPlan {
            midpoint: usize::MAX,
            ..plan
        },
        StepPlan {
            fixed: usize::MAX,
            common: 0,
            reciprocal: 0,
            midpoint: 0,
        },
    ];
    for plan in plans {
        for count in [0, 1, 2, 16, 31, 32] {
            let expected = plan.fixed as u128
                + SAMPLES as u128 * plan.common as u128
                + count as u128 * plan.reciprocal as u128
                + (SAMPLES - count) as u128 * plan.midpoint as u128;
            assert_eq!(plan.steps(count), usize::try_from(expected).ok());
        }
        for count in [SAMPLES + 1, u32::MAX] {
            assert_eq!(plan.steps(count), None);
        }
    }
}

/// State carries across two calls of one immutable compiled leaf; both paths,
/// both signs, ±0 history bits and data-dependent steps are exercised.
#[test]
fn two_blocks_carry_state_through_both_paths() {
    let input: Vec<f32> = (0..2 * N)
        .map(|n| match n % 5 {
            0 => -0.0,
            1 => 0.3 * (n as f32 - 30.0),
            2 => 0.3 * (n as f32 - 31.0) + 3e-5,
            3 => -1.7,
            _ => 1e-5 * n as f32,
        })
        .chain([-0.0])
        .skip(1)
        .collect();
    let (expected, paths) = reference(-0.0, &input);
    let (compiled, mut memory) = fixture(-0.0, &input[..N]);
    let code_before = compiled.code().clone();
    let mut state = ScalarState::default();
    let mut host = OwnedAdaaClip::new(Preactivation::new(-0.0).unwrap());

    for block in 0..2 {
        let range = block * N..(block + 1) * N;
        for (offset, value) in input[range.clone()].iter().enumerate() {
            memory
                .write(dm(0x100 + offset as u32), value.to_bits())
                .unwrap();
        }
        let receipt = compiled
            .simulate_atomic(&mut state, &mut memory, BUDGET)
            .unwrap();
        let reciprocal = paths[range.clone()].iter().filter(|&&q| q).count() as u32;
        assert!(
            0 < reciprocal && reciprocal < SAMPLES,
            "both paths in block {block}"
        );
        assert_eq!(
            receipt,
            Receipt {
                steps: compiled.step_plan().steps(reciprocal).unwrap(),
                stores: N + 1,
                reciprocal_samples: reciprocal,
            }
        );
        assert_eq!(
            bits(&memory, 0x140, SAMPLES),
            to_bits(&expected[range.clone()])
        );
        assert_eq!(
            host_block(&mut host, &input[range.clone()]).map(|out| to_bits(&out)),
            Ok(to_bits(&expected[range]))
        );
    }
    // History is the raw bits of the last sample, here -0.
    assert_eq!(memory.read(dm(0x180)), Ok(0x8000_0000));
    assert_eq!(host.previous().get().to_bits(), 0x8000_0000);
    assert_eq!(compiled.code(), &code_before);
    let plan = compiled.step_plan();
    assert_eq!((plan.reciprocal, plan.midpoint), (28, 8));
}

/// `|d| == eps` takes the midpoint (equality included); the next float above takes
/// the quotient. One quotient differs from `reference::AdaaClip` (Rust division),
/// which proves the owned reciprocal, not division, produced the output.
#[test]
fn epsilon_threshold_and_owned_quotient_differs_from_division() {
    let above = f32::from_bits(ADAA_EPSILON.to_bits() + 1);
    let differs = f32::from_bits(0x3e99_acaf);
    let mut input = vec![ADAA_EPSILON, 0.0, -ADAA_EPSILON, 0.0, above, 0.25, differs];
    input.resize(N, differs);
    let (expected, paths) = reference(0.0, &input);
    assert_eq!(
        &paths[..8],
        [false, false, false, false, true, true, true, false]
    );

    let (compiled, mut memory) = fixture(0.0, &input);
    let receipt = compiled
        .simulate_atomic(&mut ScalarState::default(), &mut memory, BUDGET)
        .unwrap();
    assert_eq!(receipt.reciprocal_samples, 3);
    let output = bits(&memory, 0x140, SAMPLES);
    assert_eq!(output, to_bits(&expected));
    let midpoint = ADAA_EPSILON * 0.5;
    assert_eq!(output[0], (midpoint * (1.0 - midpoint * 0.25)).to_bits());

    let mut division = AdaaClip::new(0.25).unwrap();
    assert_eq!(division.process(differs).unwrap().to_bits(), 0x3e83_2013);
    assert_eq!(output[6], 0x3e83_2012);
}

/// Exact input/output alias is allowed; partial overlap and state overlap are not.
/// One-past endpoints must fit u32 DM and the code must fit 24-bit PM.
#[test]
fn layout_alias_and_top_address_rules() {
    let input: Vec<f32> = (0..N).map(|n| (n as f32 - 16.0) * 0.21).collect();
    let (expected, _) = reference(0.5, &input);

    let aliased = Layout::new(dm(0x100), dm(0x100), dm(0x180)).unwrap();
    let (_, mut memory) = fixture(0.5, &input);
    CompiledAdaaClip::compile(origin(), aliased)
        .unwrap()
        .simulate_atomic(&mut ScalarState::default(), &mut memory, BUDGET)
        .unwrap();
    assert_eq!(bits(&memory, 0x100, SAMPLES), to_bits(&expected));

    for (input, output, state, pair) in [
        (0x100, 0x101, 0x180, (0, 1)),
        (0x120, 0x101, 0x180, (0, 1)),
        (0x100, 0x140, 0x11f, (0, 2)),
        (0x100, 0x140, 0x140, (1, 2)),
    ] {
        assert_eq!(
            Layout::new(dm(input), dm(output), dm(state)),
            Err(Error::Overlap {
                first: pair.0,
                second: pair.1
            })
        );
    }

    let top = u32::MAX - 32;
    assert_eq!(
        Layout::new(dm(top + 1), dm(0), dm(0x40)),
        Err(Error::Address(AddressError::DmOverflow))
    );
    assert_eq!(
        Layout::new(dm(0), dm(0x40), dm(u32::MAX)),
        Err(Error::Address(AddressError::DmOverflow))
    );
    let layout = Layout::new(dm(top), dm(top), dm(top - 1)).unwrap();
    let mut words = vec![0.5f32.to_bits()];
    words.extend(input.iter().map(|value| value.to_bits()));
    let mut memory = DmMemory::new(dm(top - 1), words).unwrap();
    let compiled = CompiledAdaaClip::compile(origin(), layout).unwrap();
    compiled
        .simulate_atomic(&mut ScalarState::default(), &mut memory, BUDGET)
        .unwrap();
    assert_eq!(bits(&memory, top, SAMPLES), to_bits(&expected));

    let words = compiled.code().word_count() as u32;
    let last = PmWordAddress24::new(0x100_0000 - words).unwrap();
    assert!(CompiledAdaaClip::compile(last, layout).is_ok());
    let beyond = PmWordAddress24::new(0x100_0000 - words + 1).unwrap();
    assert!(matches!(
        CompiledAdaaClip::compile(beyond, layout),
        Err(Error::Emit(EmitError::ProgramBounds { .. }))
    ));
}

/// Budget, mode, length, range, domain and strict-numeric refusals leave registers,
/// DM and the write log untouched; a corrected retry then commits normally.
#[test]
fn refusals_are_atomic_and_retry_succeeds() {
    let tiny = f32::from_bits(0x0380_0000); // 2^-120, normal
    let mut input: Vec<f32> = (0..N).map(|n| (n as f32 * 0.37).sin()).collect();
    let (compiled, clean) = fixture(0.1, &input);
    let steps = {
        let mut memory = clean.clone();
        compiled
            .simulate_atomic(&mut ScalarState::default(), &mut memory, BUDGET)
            .unwrap()
            .steps
    };

    let check = |memory: DmMemory, state: ScalarState, budget: usize, expected: Error| {
        let (mut next_memory, mut next_state) = (memory.clone(), state.clone());
        assert_eq!(
            compiled.simulate_atomic(&mut next_state, &mut next_memory, budget),
            Err(expected)
        );
        assert_eq!((next_memory, next_state), (memory, state));
    };
    check(
        clean.clone(),
        ScalarState::default(),
        steps - 1,
        SimulationError::StepBudget(steps - 1).into(),
    );
    let mut state = ScalarState::default();
    state.lengths[1] = 4;
    check(
        clean.clone(),
        state,
        BUDGET,
        SimulationError::CircularLength {
            index: 1,
            length: 4,
        }
        .into(),
    );
    let short = DmMemory::new(dm(0x100), clean.words()[..0x80].to_vec()).unwrap();
    check(
        short,
        ScalarState::default(),
        BUDGET,
        SimulationError::MemoryBounds {
            address: 0x180,
            words: 1,
        }
        .into(),
    );
    for (address, value) in [
        (0x105, f32::NAN),
        (0x105, f32::INFINITY),
        (0x105, f32::from_bits(1)),
        (0x105, 2f32.powi(65)),
        (0x180, f32::MIN_POSITIVE / 2.0),
    ] {
        let mut memory = clean.clone();
        memory.write(dm(address), value.to_bits()).unwrap();
        check(
            memory,
            ScalarState::default(),
            BUDGET,
            Error::DomainAt {
                address,
                bits: value.to_bits(),
            },
        );
    }

    // Two normal neighbours cancel to a subnormal d = 2^-143. No flush: refused.
    input[6] = tiny;
    input[7] = f32::from_bits(tiny.to_bits() + 1);
    let (_, poisoned) = fixture(0.1, &input);
    let refusal = {
        let mut memory = poisoned.clone();
        compiled
            .simulate_atomic(&mut ScalarState::default(), &mut memory, BUDGET)
            .unwrap_err()
    };
    assert!(matches!(
        refusal,
        Error::Simulation(SimulationError::NonNormalFloat { .. })
    ));
    check(poisoned, ScalarState::default(), BUDGET, refusal);
    let mut host = OwnedAdaaClip::new(Preactivation::new(0.1).unwrap());
    assert_eq!(
        host_block(&mut host, &input),
        Err(Error::NonNormal { sample: Some(7) })
    );
    assert_eq!(host.previous().get(), 0.1);

    let mut memory = clean;
    let mut state = ScalarState::default();
    let receipt = compiled
        .simulate_atomic(&mut state, &mut memory, steps)
        .unwrap();
    assert_eq!((receipt.steps, receipt.stores), (steps, N + 1));
    assert_eq!(memory.writes().len(), N + 1);
}

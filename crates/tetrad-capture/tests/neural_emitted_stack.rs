//! Owned emitted 1→2→2→1 neural stack: 11 raw ISA48 leaf calls under the normal-only runner against
//! the public owned host model and an independent std-f32 oracle written from the contract.

use tetrad_capture::network::emitted::{Error as AdaaError, OwnedAdaaClip, Preactivation};
use tetrad_capture::network::emitted_stack::{
    EmittedNeuralStack, Error, LEAVES, Leaf, LeafPlacement, LeafReceipt, NeuralStackBlock,
    OwnedNeuralStack,
};
use tetrad_capture::network::{NetworkParameters, NeuralCore};
use tetrad_capture::reference::{ADAA_EPSILON, AdaaClip};
use tetrad_sharc::address::{AddressError, DmWordAddress32, PmWordAddress24};
use tetrad_sharc::simulator::SimulationError;

const N: usize = 32;

/// Plain tensors so the oracle reads the same numbers as `NetworkParameters`.
#[derive(Clone, Copy)]
struct Weights {
    first_w: [f32; 2],
    first_b: [f32; 2],
    middle_w: [[f32; 2]; 2],
    middle_b: [f32; 2],
    last_w: [f32; 2],
    last_b: f32,
    beta: f32,
}

impl Weights {
    fn network(self) -> NetworkParameters {
        NetworkParameters::new(
            self.first_w,
            self.first_b,
            self.middle_w,
            self.middle_b,
            self.last_w,
            self.last_b,
            self.beta,
        )
        .unwrap()
    }
}

const ASYMMETRIC: Weights = Weights {
    first_w: [0.5, -0.25],
    first_b: [0.25, -0.5],
    middle_w: [[0.75, -0.5], [0.25, 0.625]],
    middle_b: [0.125, -0.0625],
    last_w: [0.5, -0.375],
    last_b: 0.125,
    beta: 1.75,
};

/// Independent std-f32 oracle of the owned contract: stock bias-first affines,
/// ADAA with seed `0x7EF311C7 - bits(m)` and 7 Newton steps, `beta*y + (-baseline)`.
struct Oracle {
    w: Weights,
    history: [f32; 5],
    offset: f32,
}

fn zero_clip(v: f32) -> f32 {
    let c = v.clamp(-2.0, 2.0);
    c * (1.0 - c.abs() * 0.25)
}

fn affine(w: [f32; 2], b: f32, h: [f32; 2]) -> f32 {
    (w[0] * h[0] + b) + w[1] * h[1]
}

fn adaa(previous: &mut f32, input: [f32; N]) -> [f32; N] {
    let a = |v: f32| {
        let b = v.abs().min(2.0);
        (((f32::from_bits(0xbdaa_aaab) * b + 0.5) * b - 1.0) * b) + v.abs()
    };
    input.map(|x| {
        let p = *previous;
        *previous = x;
        let d = x - p;
        let m = d.abs().max(ADAA_EPSILON);
        if m == ADAA_EPSILON {
            return zero_clip((x + p) * 0.5);
        }
        let mut r = f32::from_bits(0x7ef3_11c7 - m.to_bits());
        for _ in 0..7 {
            r *= 2.0 - m * r;
        }
        ((a(x) - a(p)) * 1.0f32.copysign(d)) * r
    })
}

impl Oracle {
    fn new(w: Weights) -> Self {
        let z = w.first_b.map(zero_clip);
        let middle = [0, 1].map(|r| affine(w.middle_w[r], w.middle_b[r], z));
        let last = affine(w.last_w, w.last_b, middle.map(zero_clip));
        Self {
            w,
            history: [w.first_b[0], w.first_b[1], middle[0], middle[1], last],
            offset: -(w.beta * zero_clip(last)),
        }
    }

    fn block(&mut self, x: [f32; N]) -> [f32; N] {
        let (w, h) = (self.w, &mut self.history);
        let [h0, h1, h2, h3, h4] = h;
        let a0 = adaa(h0, x.map(|x| w.first_w[0] * x + w.first_b[0]));
        let a1 = adaa(h1, x.map(|x| w.first_w[1] * x + w.first_b[1]));
        let mid = |r: usize| {
            std::array::from_fn(|n| affine(w.middle_w[r], w.middle_b[r], [a0[n], a1[n]]))
        };
        let m0 = adaa(h2, mid(0));
        let m1 = adaa(h3, mid(1));
        let y = adaa(
            h4,
            std::array::from_fn(|n| affine(w.last_w, w.last_b, [m0[n], m1[n]])),
        );
        y.map(|y| w.beta * y + self.offset)
    }
}

/// Distinct logical origins and DM bases; `spread = false` makes all 11 coincide.
fn placements(spread: bool) -> [LeafPlacement; LEAVES] {
    std::array::from_fn(|i| {
        let i = if spread { i as u32 } else { 0 };
        LeafPlacement {
            origin: PmWordAddress24::new(0x2_0000 + 0x400 * i).unwrap(),
            dm_base: DmWordAddress32::new(0x8000 + 0x100 * i),
        }
    })
}

fn signal(block: usize) -> [f32; N] {
    std::array::from_fn(|n| {
        let t = (block * N + n) as f32;
        match n % 7 {
            0 => 0.0,
            1 => -0.0,
            _ => 1.9 * (0.37 * t).sin() - 0.4 * (0.05 * t).cos(),
        }
    })
}

fn bits<const K: usize>(values: [f32; K]) -> [u32; K] {
    values.map(f32::to_bits)
}

fn history_bits(stack: &EmittedNeuralStack) -> [u32; 5] {
    stack.histories().map(|p| p.get().to_bits())
}

fn steps(receipt: LeafReceipt) -> usize {
    match receipt {
        LeafReceipt::Linear(outcome) => outcome.steps,
        LeafReceipt::Adaa(receipt) => receipt.steps,
    }
}

fn stack(w: Weights) -> EmittedNeuralStack {
    EmittedNeuralStack::new(w.network(), placements(true)).unwrap()
}

/// All three layers run ADAA through both paths over three blocks; emitted,
/// owned host and the independent oracle agree bitwise, and the receipts are the
/// actual per-leaf decoder counts of the encoded images.
#[test]
fn three_adaa_layers_match_owned_host_and_oracle_with_actual_receipts() {
    let mut emitted = stack(ASYMMETRIC);
    let mut host = OwnedNeuralStack::new(ASYMMETRIC.network()).unwrap();
    let mut oracle = Oracle::new(ASYMMETRIC);
    assert_eq!(history_bits(&emitted), bits(oracle.history));
    let images: Vec<_> = Leaf::ORDER.map(|leaf| emitted.code(leaf).clone()).into();
    let budgets = emitted.full_budgets();
    assert_eq!(
        budgets,
        [131, 131, 1556, 1556, 228, 228, 1556, 1556, 228, 1556, 131]
    );
    let mut quotient = [0u32; 5];
    let mut midpoint = [0u32; 5];
    for block in 0..3 {
        let input = signal(block);
        let out = emitted.process_block(&input, &budgets).unwrap();
        let expected = bits(oracle.block(input));
        assert_eq!(bits(out.outputs), expected, "block {block}");
        assert_eq!(bits(host.process_block(&input).unwrap()), expected);
        assert_eq!(history_bits(&emitted), bits(oracle.history));

        let mut adaa = 0;
        for (leaf, receipt) in Leaf::ORDER.into_iter().zip(out.receipts) {
            let words = emitted.code(leaf).word_count();
            match receipt {
                LeafReceipt::Linear(outcome) => {
                    assert_eq!((outcome.steps, outcome.stores), (words, N), "{leaf:?}");
                }
                LeafReceipt::Adaa(receipt) => {
                    assert_eq!(words, 76);
                    let r = receipt.reciprocal_samples;
                    assert_eq!(receipt.steps, 916 + 20 * r as usize, "{leaf:?}");
                    assert_eq!(receipt.stores, N + 1);
                    quotient[adaa] += r;
                    midpoint[adaa] += 32 - r;
                    adaa += 1;
                }
            }
        }
        assert_eq!(adaa, 5);
    }
    // Every neuron of every layer took both the quotient and the midpoint path.
    assert!(quotient.iter().chain(&midpoint).all(|&count| count > 0));
    assert_eq!(
        Leaf::ORDER.map(|leaf| emitted.code(leaf).clone()).to_vec(),
        images
    );
}

/// Products 1 and -1 with bias 2^-30 give +0 bias-first, 2^-30 otherwise. The
/// seed and the emitted middle affine (carried as raw history) both show +0;
/// zero input then leaves exactly +0 output against the constant baseline.
#[test]
fn bias_first_cancellation_and_zero_input_baseline() {
    let w = Weights {
        first_b: [2.0, -2.0], // zero clip: 1.0 and -1.0
        middle_w: [[1.0, 1.0], [0.5, -0.25]],
        middle_b: [f32::from_bits(0x3080_0000), 0.375],
        ..ASYMMETRIC
    };
    let mut emitted = stack(w);
    assert_eq!(history_bits(&emitted)[2], 0);
    assert_eq!(
        (1.0f32 + -1.0) + f32::from_bits(0x3080_0000),
        2f32.powi(-30)
    );
    let budgets = emitted.full_budgets();
    for _ in 0..2 {
        let out = emitted.process_block(&[0.0; N], &budgets).unwrap();
        assert_eq!(bits(out.outputs), [0; N]);
        assert_eq!(history_bits(&emitted)[2], 0, "emitted raw middle0");
        // Zero input never leaves the midpoint path.
        for receipt in out.receipts {
            if let LeafReceipt::Adaa(receipt) = receipt {
                assert_eq!(receipt.reciprocal_samples, 0);
            }
        }
    }

    // Zero first weights: any input gives the zero-input constant, so 0 output.
    let constant = Weights {
        first_w: [0.0; 2],
        ..ASYMMETRIC
    };
    let mut emitted = stack(constant);
    let out = emitted
        .process_block(&signal(1), &emitted.full_budgets())
        .unwrap();
    assert_eq!(bits(out.outputs), [0; N]);
}

/// The owned reciprocal runs, not Rust division: the stack matches its owned
/// models bitwise, while `NeuralCore` is only close. It is not an oracle.
#[test]
fn owned_quotient_differs_from_division_core() {
    // Leaf witness: p = 0.25, x = 0x3e99acaf.
    let x = f32::from_bits(0x3e99_acaf);
    let mut owned = OwnedAdaaClip::new(Preactivation::new(0.25).unwrap());
    let mut block = [Preactivation::new(x).unwrap(); N];
    block[1..].fill(Preactivation::new(0.25).unwrap());
    assert_eq!(
        owned.process_block(&block).unwrap().output[0].to_bits(),
        0x3e83_2012
    );
    assert_eq!(
        AdaaClip::new(0.25).unwrap().process(x).unwrap().to_bits(),
        0x3e83_2013
    );

    // Stack: first bias 0.25 and unit weight put that pair into first0's sample 0.
    let w = Weights {
        first_w: [1.0, -0.25],
        first_b: [0.25, -0.5],
        ..ASYMMETRIC
    };
    let mut input = signal(0);
    input[0] = x - 0.25;
    assert_eq!((input[0] + 0.25).to_bits(), x.to_bits());
    let mut emitted = stack(w);
    let out = emitted
        .process_block(&input, &emitted.full_budgets())
        .unwrap()
        .outputs;
    assert_eq!(bits(out), bits(Oracle::new(w).block(input)));
    let mut division = [0.0; N];
    NeuralCore::new(w.network())
        .unwrap()
        .process_chunk(&input, &mut division)
        .unwrap();
    assert_ne!(bits(out), bits(division));
    // Near-eps cancellation amplifies quotient ulps (~2e-5 here); a loose sanity
    // bound, not an accuracy claim.
    assert!(out.iter().zip(division).all(|(a, b)| (a - b).abs() < 1e-4));
}

/// Budget refusal at a leaf in each layer and in the scale leaf, using the actual
/// receipt minus one, proves that leaf's encoded words execute. Every refusal and
/// a scale overflow retain all five histories; retries are bitwise identical.
#[test]
fn late_budget_and_scale_failures_are_atomic_and_retry_bitwise() {
    let mut emitted = stack(ASYMMETRIC);
    let full = emitted.full_budgets();
    emitted.process_block(&signal(0), &full).unwrap();
    let input = signal(1);
    let expected = emitted.clone().process_block(&input, &full).unwrap();
    let before = history_bits(&emitted);
    for leaf in [
        Leaf::FirstAffine0,
        Leaf::FirstAdaa1,
        Leaf::MiddleAffine1,
        Leaf::MiddleAdaa0,
        Leaf::LastAffine,
        Leaf::LastAdaa,
        Leaf::Scale,
    ] {
        let mut budgets = full;
        budgets[leaf as usize] = steps(expected.receipts[leaf as usize]) - 1;
        assert_eq!(
            emitted.process_block(&input, &budgets),
            Err(Error::Leaf {
                leaf,
                error: AdaaError::Simulation(SimulationError::StepBudget(budgets[leaf as usize])),
            })
        );
        assert_eq!(history_bits(&emitted), before, "{leaf:?}");
    }
    let exact = expected.receipts.map(steps);
    let retry: NeuralStackBlock = emitted.process_block(&input, &exact).unwrap();
    assert_eq!(bits(retry.outputs), bits(expected.outputs));
    assert_eq!(retry.receipts, expected.receipts);

    // beta = MAX with zero_last = 1 gives baseline 0.75*MAX; a negative last
    // output then overflows `beta*y + (-baseline)` inside the scale leaf only.
    let w = Weights {
        first_w: [4.0, 0.0],
        first_b: [0.0; 2],
        middle_w: [[1.0, 0.0], [0.0; 2]],
        middle_b: [0.0; 2],
        last_w: [8.0, 0.0],
        last_b: 1.0,
        beta: f32::MAX,
    };
    let mut emitted = stack(w);
    let full = emitted.full_budgets();
    emitted.process_block(&[0.0; N], &full).unwrap();
    let before = history_bits(&emitted);
    let expected = emitted.clone().process_block(&[0.0; N], &full).unwrap();
    let result = emitted.process_block(&[-1.0; N], &full);
    assert!(
        matches!(
            result,
            Err(Error::Leaf {
                leaf: Leaf::Scale,
                error: AdaaError::Simulation(SimulationError::NonNormalFloat { .. }),
            })
        ),
        "{result:?}"
    );
    assert!(matches!(
        OwnedNeuralStack::new(w.network())
            .unwrap()
            .process_block(&[-1.0; N]),
        Err(Error::Leaf {
            leaf: Leaf::Scale,
            ..
        })
    ));
    assert_eq!(history_bits(&emitted), before);
    assert_eq!(emitted.process_block(&[0.0; N], &full).unwrap(), expected);
}

/// Reset restores seeds with the same images; reload rebuilds parameter
/// immediates at retained origins; failed reloads keep everything; instances
/// and coinciding private placements do not interact.
#[test]
fn reset_reload_instances_and_placement_retention() {
    let full = stack(ASYMMETRIC).full_budgets();
    let mut a = stack(ASYMMETRIC);
    let mut b = EmittedNeuralStack::new(ASYMMETRIC.network(), placements(false)).unwrap();
    let seeds = history_bits(&a);
    let first = a.process_block(&signal(0), &full).unwrap().outputs;
    a.process_block(&signal(1), &full).unwrap();
    assert_eq!(
        bits(b.process_block(&signal(0), &full).unwrap().outputs),
        bits(first)
    );
    a.reset();
    assert_eq!(history_bits(&a), seeds);
    assert_eq!(
        bits(a.process_block(&signal(0), &full).unwrap().outputs),
        bits(first)
    );

    let other = Weights {
        middle_b: [-0.25, 0.5],
        beta: 0.5,
        ..ASYMMETRIC
    };
    let images = Leaf::ORDER.map(|leaf| a.code(leaf).clone());
    a.reload(other.network()).unwrap();
    assert_eq!(history_bits(&a), bits(Oracle::new(other).history));
    for (leaf, (image, placement)) in Leaf::ORDER
        .into_iter()
        .zip(images.iter().zip(placements(true)))
    {
        let code = a.code(leaf);
        assert_eq!(code.origin(), placement.origin);
        let changed = matches!(
            leaf,
            Leaf::MiddleAffine0 | Leaf::MiddleAffine1 | Leaf::Scale
        );
        assert_eq!(code != image, changed, "{leaf:?}");
    }
    assert_eq!(
        bits(a.process_block(&signal(0), &full).unwrap().outputs),
        bits(Oracle::new(other).block(signal(0)))
    );

    let kept = a.clone();
    let subnormal = Weights {
        last_w: [f32::from_bits(1), 0.5],
        ..ASYMMETRIC
    };
    assert_eq!(a.reload(subnormal.network()), Err(Error::Coefficient(1)));
    let out_of_domain = Weights {
        first_b: [2f32.powi(65), 0.0],
        ..ASYMMETRIC
    };
    assert_eq!(a.reload(out_of_domain.network()), Err(Error::Seed));
    assert_eq!(history_bits(&a), history_bits(&kept));
    let mut kept = kept;
    assert_eq!(
        a.process_block(&signal(2), &full).unwrap(),
        kept.process_block(&signal(2), &full).unwrap()
    );
}

/// Refusals happen at actual operations or before the ADAA call that would see
/// the value; admitted input is not a proof that later work stays normal.
#[test]
fn normal_domain_and_range_refusals() {
    let mut emitted = stack(ASYMMETRIC);
    let full = emitted.full_budgets();
    let before = history_bits(&emitted);
    let first_affine = |result: Result<NeuralStackBlock, Error>| {
        matches!(
            result,
            Err(Error::Leaf {
                leaf: Leaf::FirstAffine0,
                error: AdaaError::Simulation(SimulationError::NonNormalFloat { .. }),
            })
        )
    };
    let mut input = signal(0);
    input[5] = f32::from_bits(1);
    assert!(first_affine(emitted.process_block(&input, &full)));
    input[5] = f32::NAN;
    assert!(first_affine(emitted.process_block(&input, &full)));
    input[5] = 2f32.powi(70); // 0.5*x + 0.25 = 2^69 > 2^64
    assert_eq!(
        emitted.process_block(&input, &full),
        Err(Error::Leaf {
            leaf: Leaf::FirstAdaa0,
            error: AdaaError::Domain(2f32.powi(69).to_bits()),
        })
    );
    let mut host = OwnedNeuralStack::new(ASYMMETRIC.network()).unwrap();
    assert_eq!(
        host.process_block(&input),
        Err(Error::Leaf {
            leaf: Leaf::FirstAdaa0,
            error: AdaaError::Domain(2f32.powi(69).to_bits()),
        })
    );
    input[5] = f32::from_bits(1);
    assert_eq!(
        host.process_block(&input),
        Err(Error::Leaf {
            leaf: Leaf::FirstAffine0,
            error: AdaaError::NonNormal { sample: Some(5) },
        })
    );
    assert_eq!(history_bits(&emitted), before);

    // Normal weight 2^-126 times an ADAA output below 1 is subnormal in the middle.
    let tiny = Weights {
        first_b: [0.0; 2],
        middle_w: [[f32::MIN_POSITIVE, 0.5], [0.25, 0.625]],
        ..ASYMMETRIC
    };
    let mut emitted = stack(tiny);
    emitted.process_block(&[0.0; N], &full).unwrap();
    assert!(matches!(
        emitted.process_block(&signal(0), &full),
        Err(Error::Leaf {
            leaf: Leaf::MiddleAffine0,
            error: AdaaError::Simulation(SimulationError::NonNormalFloat { .. }),
        })
    ));

    let tiny_seed = Weights {
        first_b: [2f32.powi(-125), 0.0], // |c|*0.25 is subnormal
        ..ASYMMETRIC
    };
    assert!(matches!(
        EmittedNeuralStack::new(tiny_seed.network(), placements(true)),
        Err(Error::Seed)
    ));
    let mut overflow = placements(true);
    overflow[Leaf::FirstAdaa1 as usize].dm_base = DmWordAddress32::new(u32::MAX - 40);
    assert!(matches!(
        EmittedNeuralStack::new(ASYMMETRIC.network(), overflow),
        Err(Error::Leaf {
            leaf: Leaf::FirstAdaa1,
            error: AdaaError::Address(AddressError::DmOverflow),
        })
    ));
}

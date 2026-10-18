//! Complete owned emitted V1 stage order against an explicit host composition of
//! the public leaves. Synthetic records and tables only; no stock fixtures.

use tetrad_capture::{
    Error as CaptureError,
    filters::{
        FirLogicalPlacement, HighPassLogicalPlacement, V1FirReference, V1HighPassReference,
        emitted::high_pass, v1_high_pass_coefficients_reference,
    },
    gain::{self, GainAdapter},
    network::{
        emitted::Error as NeuralLeafError,
        emitted_stack::{self, Leaf, LeafPlacement, LeafReceipt, OwnedNeuralStack},
    },
    pipeline::{
        V1StagePlacements,
        emitted::{
            AlphaError, EmittedV1PathBudgets, EmittedV1PathPlacements, Error, Half,
            V1EmittedSignalPathReference,
        },
    },
    reference::GainSmoother,
    resampling::{
        self, CallerOwnedV1ResamplingTable, V1ResamplingReference,
        emitted::ResamplingLogicalPlacement,
    },
    tones::{
        self, V1ToneStackReference, emitted::ToneLogicalPlacement,
        emitted_stack::EmittedV1ToneStackReference,
    },
    v1::{Limits, RuntimeData},
};
use tetrad_sharc::{
    address::{AddressError, DmWordAddress32, PmWordAddress24},
    fir::FirError,
    simulator::{SimulationError, SimulationOutcome},
};

const ALPHA: usize = 0x280c;
const BETA: usize = 0x2810;
const TONES: [f32; 3] = [0.9, 0.2, 0.75];

fn put(bytes: &mut [u8], offset: usize, value: f32) {
    bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

/// The pipeline unit-test record with an active asymmetric high-pass.
fn record_bytes() -> Vec<u8> {
    let mut bytes = vec![0; 0x2864];
    for (offset, word) in [
        (4, 512_u32),
        (8, 2048),
        (0x2814, 3),
        (0x2818, 1),
        (0x281c, 2),
        (0x2820, 2),
        (0x2824, 1),
    ] {
        bytes[offset..offset + 4].copy_from_slice(&word.to_le_bytes());
    }
    for (offset, value) in [
        (0, 48_000.0),
        (0xc, 3.0),
        (0x10, 4.0),
        (0x80c, 2.0),
        (0x810, -0.5),
        (ALPHA, 1.5),
        (BETA, 1.75),
        (0x285c, 1200.0),
        (0x2860, 2.0),
    ] {
        put(&mut bytes, offset, value);
    }
    for (index, value) in [
        0.5, -0.25, 0.25, -0.5, 0.75, -0.5, 0.25, 0.625, 0.125, -0.0625, 0.5, -0.375, 0.125,
    ]
    .into_iter()
    .enumerate()
    {
        put(&mut bytes, 0x2828 + 4 * index, value);
    }
    bytes
}

fn parse(bytes: &[u8]) -> RuntimeData<'_> {
    RuntimeData::parse(bytes, Limits::default()).unwrap()
}

fn table(scale: f32, provenance: &str) -> CallerOwnedV1ResamplingTable {
    let coefficients = std::array::from_fn(|j| scale * [0.5, 0.25, -0.125, 0.0625][j % 4]);
    CallerOwnedV1ResamplingTable::new(coefficients, provenance.into()).unwrap()
}

fn pm(origin: u32) -> PmWordAddress24 {
    PmWordAddress24::new(origin).unwrap()
}

/// Every DM base coincides: each leaf call stages private DM.
fn placements(dm: u32) -> EmittedV1PathPlacements {
    let dm_base = DmWordAddress32::new(dm);
    let hp = HighPassLogicalPlacement {
        origin: pm(0x1000),
        dm_base,
    };
    let fir = FirLogicalPlacement {
        origin: pm(0x2000),
        dm_base,
    };
    EmittedV1PathPlacements {
        stages: V1StagePlacements {
            input_hp: hp,
            pre: fir,
            post: fir,
            output_hp: hp,
        },
        interpolate: ResamplingLogicalPlacement {
            origin: pm(0x4000),
            dm_base,
        },
        alpha: LeafPlacement {
            origin: pm(0x5000),
            dm_base,
        },
        neural: std::array::from_fn(|i| LeafPlacement {
            origin: pm(0x6000 + 0x400 * i as u32),
            dm_base,
        }),
        decimate: ResamplingLogicalPlacement {
            origin: pm(0x9000),
            dm_base,
        },
    }
}

fn gain(current: f32, target: f32) -> GainAdapter {
    GainAdapter::new(pm(0x800), DmWordAddress32::new(0x8000), current, target).unwrap()
}

fn emitted_tones(normalized: [f32; 3]) -> EmittedV1ToneStackReference {
    let placement = ToneLogicalPlacement {
        origin: pm(0xc00),
        dm_base: DmWordAddress32::new(0x8000),
    };
    EmittedV1ToneStackReference::new(normalized, [placement; 3]).unwrap()
}

fn path(bytes: &[u8], provenance: &str) -> V1EmittedSignalPathReference {
    V1EmittedSignalPathReference::new(
        &parse(bytes),
        table(1.0, provenance),
        placements(0x8000),
        gain(0.125, 1.75),
        emitted_tones(TONES),
        gain(2.0, 0.375),
    )
    .unwrap()
}

fn signal(frame: usize) -> [f32; 32] {
    std::array::from_fn(|n| ((n * 7 + frame * 3) % 23) as f32 / 32.0 - 0.375)
}

/// Debug covers every gain, history, cursor, seed, image and table bit.
fn assert_same(actual: &V1EmittedSignalPathReference, expected: &V1EmittedSignalPathReference) {
    assert_eq!(format!("{actual:?}"), format!("{expected:?}"));
}

/// Host composition of public leaves; the neural oracle is `OwnedNeuralStack`
/// (same owned reciprocal), never `NeuralCore`.
struct Oracle {
    input_gain: GainSmoother,
    input_hp: V1HighPassReference,
    pre: V1FirReference<512>,
    resampler: V1ResamplingReference,
    alpha: f32,
    neural: OwnedNeuralStack,
    post: V1FirReference<2048>,
    output_hp: V1HighPassReference,
    tones: V1ToneStackReference,
    output_gain: GainSmoother,
}

impl Oracle {
    fn new(record: &RuntimeData<'_>, table: CallerOwnedV1ResamplingTable) -> Self {
        let [frequency, bandwidth] = record.high_pass();
        let hp = v1_high_pass_coefficients_reference(frequency, bandwidth).unwrap();
        let [alpha, beta] = record.constant_gains();
        let pre = std::array::from_fn(|i| record.pre_coefficients().get(i).unwrap());
        let post = std::array::from_fn(|i| record.post_coefficients().get(i).unwrap());
        Self {
            input_gain: GainSmoother::from_state(0.125, 1.75).unwrap(),
            input_hp: V1HighPassReference::new(hp).unwrap(),
            pre: V1FirReference::from_imported_prefilter(&pre).unwrap(),
            resampler: V1ResamplingReference::new(table),
            alpha,
            neural: OwnedNeuralStack::new(record.network_parameters(beta).unwrap()).unwrap(),
            post: V1FirReference::from_postfilter(post).unwrap(),
            output_hp: V1HighPassReference::new(hp).unwrap(),
            tones: V1ToneStackReference::from_normalized(TONES).unwrap(),
            output_gain: GainSmoother::from_state(2.0, 0.375).unwrap(),
        }
    }

    fn process(&mut self, inputs: &[f32; 32]) -> [f32; 32] {
        let gained = inputs.map(|x| self.input_gain.process(x).unwrap());
        let filtered = self.input_hp.process_block(&gained).unwrap();
        let pre = self.pre.process_block(&filtered).unwrap();
        let interpolated = self.resampler.interpolate_block(&pre).unwrap();
        let mut neural = [0.0; 64];
        for (input, output) in interpolated
            .chunks_exact(32)
            .zip(neural.chunks_exact_mut(32))
        {
            let scaled = std::array::from_fn(|n| self.alpha * input[n]);
            output.copy_from_slice(&self.neural.process_block(&scaled).unwrap());
        }
        let decimated = self.resampler.decimate_block(&neural).unwrap();
        let post = self.post.process_block(&decimated).unwrap();
        let filtered = self.output_hp.process_block(&post).unwrap();
        let toned = self.tones.process_block(&filtered).unwrap();
        toned.map(|x| self.output_gain.process(x).unwrap())
    }
}

/// Every straight-line leaf runs its whole image; each ADAA leaf stores 32 outputs
/// plus its history and stays within the all-quotient maximum.
fn assert_receipts(path: &V1EmittedSignalPathReference, frame: &[f32; 32]) {
    let budgets = path.full_budgets();
    let receipts = path.clone().process_block(frame, budgets).unwrap().receipts;
    let run = |steps: usize, stores: usize| SimulationOutcome { steps, stores };
    assert_eq!(receipts.input_gain, run(230, 33));
    assert_eq!(receipts.input_hp, run(budgets.stages.input_hp, 96));
    assert_eq!(receipts.output_hp, run(budgets.stages.output_hp, 96));
    assert_eq!(receipts.pre, run(budgets.stages.pre, 32));
    assert_eq!(receipts.post, run(budgets.stages.post, 32));
    assert_eq!(receipts.interpolate, run(3232, 64 + 15));
    assert_eq!(receipts.decimate, run(3200, 32 + 31));
    assert_eq!(receipts.alpha, [run(98, 32); 2]);
    assert_eq!(path.alpha_code().word_count(), 98);
    assert_eq!(receipts.tones, [run(366, 32 + 4); 3]);
    assert_eq!(receipts.output_gain, run(230, 33));
    for (half, half_receipts) in receipts.neural.iter().enumerate() {
        for leaf in Leaf::ORDER {
            let budget = budgets.neural[half][leaf as usize];
            match half_receipts[leaf as usize] {
                LeafReceipt::Linear(outcome) => assert_eq!(outcome, run(budget, 32)),
                LeafReceipt::Adaa(receipt) => {
                    assert_eq!(receipt.stores, 33);
                    assert!(receipt.steps <= budget, "{leaf:?}");
                }
            }
        }
    }
}

#[test]
fn full_stage_order_matches_owned_host_composition_bit_for_bit_across_frames() {
    let bytes = record_bytes();
    let record = parse(&bytes);
    let mut path = path(&bytes, "Owned synthetic table A; not stock");
    let mut oracle = Oracle::new(&record, table(1.0, "oracle"));
    assert_eq!(path.alpha().to_bits(), 1.5f32.to_bits());
    assert_eq!(path.alpha_code().origin(), pm(0x5000));
    let mut nonzero = 0;
    for frame in 0..6 {
        if frame == 2 {
            path.set_input_gain_target(0.5).unwrap();
            oracle.input_gain.set_linear_target(0.5).unwrap();
            path.set_output_gain_target(1.25).unwrap();
            oracle.output_gain.set_linear_target(1.25).unwrap();
        }
        if frame == 4 {
            path.set_tone_parameters([0.1, 1.0, 0.0]).unwrap();
            oracle.tones.set_normalized([0.1, 1.0, 0.0]).unwrap();
        }
        let inputs = signal(frame);
        assert_receipts(&path, &inputs);
        let block = path.process_block(&inputs, path.full_budgets()).unwrap();
        let expected = oracle.process(&inputs);
        assert_eq!(block.outputs.map(f32::to_bits), expected.map(f32::to_bits));
        nonzero += expected.iter().filter(|x| **x != 0.0).count();
        assert_eq!(
            path.input_gain().current().to_bits(),
            oracle.input_gain.current().to_bits()
        );
        assert_eq!(
            path.output_gain().current().to_bits(),
            oracle.output_gain.current().to_bits()
        );
    }
    assert!(nonzero > 150, "only {nonzero} nonzero outputs");
}

type Field = fn(&mut EmittedV1PathBudgets) -> &mut usize;
type Expected = fn(SimulationError) -> Error;

#[test]
fn every_late_gate_retains_the_whole_frame_and_retry_is_bitwise() {
    let bytes = record_bytes();
    let mut path = path(&bytes, "Owned rollback table");
    path.process_block(&signal(0), path.full_budgets()).unwrap();
    let before = path.clone();
    let full = path.full_budgets();
    fn neural(half: Half, error: SimulationError) -> Error {
        Error::Neural {
            half: Some(half),
            error: emitted_stack::Error::Leaf {
                leaf: Leaf::Scale,
                error: NeuralLeafError::Simulation(error),
            },
        }
    }
    // Every stage in order; the second-half scale, decimator, post FIR, output HP,
    // third tone and output gain all fail after the whole first half committed.
    let cases: [(Field, Expected); 14] = [
        (
            |b| &mut b.input_gain,
            |e| Error::InputGain(CaptureError::Gain(gain::emitted::Error::Simulation(e))),
        ),
        (
            |b| &mut b.stages.input_hp,
            |e| Error::InputHighPass(CaptureError::HighPass(high_pass::Error::Simulation(e))),
        ),
        (
            |b| &mut b.stages.pre,
            |e| Error::PreFir(CaptureError::Fir(FirError::Simulation(e))),
        ),
        (
            |b| &mut b.interpolate,
            |e| Error::Interpolate(resampling::emitted::Error::Simulation(e)),
        ),
        (
            |b| &mut b.alpha[0],
            |e| Error::Alpha {
                half: Some(Half::First),
                error: AlphaError::Simulation(e),
            },
        ),
        (
            |b| &mut b.neural[0][Leaf::Scale as usize],
            |e| neural(Half::First, e),
        ),
        (
            |b| &mut b.alpha[1],
            |e| Error::Alpha {
                half: Some(Half::Second),
                error: AlphaError::Simulation(e),
            },
        ),
        (
            |b| &mut b.neural[1][Leaf::FirstAffine0 as usize],
            |e| Error::Neural {
                half: Some(Half::Second),
                error: emitted_stack::Error::Leaf {
                    leaf: Leaf::FirstAffine0,
                    error: NeuralLeafError::Simulation(e),
                },
            },
        ),
        (
            |b| &mut b.neural[1][Leaf::Scale as usize],
            |e| neural(Half::Second, e),
        ),
        (
            |b| &mut b.decimate,
            |e| Error::Decimate(resampling::emitted::Error::Simulation(e)),
        ),
        (
            |b| &mut b.stages.post,
            |e| Error::PostFir(CaptureError::Fir(FirError::Simulation(e))),
        ),
        (
            |b| &mut b.stages.output_hp,
            |e| Error::OutputHighPass(CaptureError::HighPass(high_pass::Error::Simulation(e))),
        ),
        (
            |b| &mut b.tones.third,
            |e| Error::Tones(CaptureError::Tone(tones::emitted::Error::Simulation(e))),
        ),
        (
            |b| &mut b.output_gain,
            |e| Error::OutputGain(CaptureError::Gain(gain::emitted::Error::Simulation(e))),
        ),
    ];
    for (field, expected) in cases {
        let mut budgets = full;
        *field(&mut budgets) -= 1;
        let limit = *field(&mut budgets);
        assert_eq!(
            path.process_block(&signal(1), budgets),
            Err(expected(SimulationError::StepBudget(limit)))
        );
        assert_same(&path, &before);
    }
    let mut invalid = signal(1);
    invalid[31] = f32::NAN;
    assert!(matches!(
        path.process_block(&invalid, full),
        Err(Error::InputGain(CaptureError::Gain(
            gain::emitted::Error::NonFiniteMemory { .. }
        )))
    ));
    assert_same(&path, &before);
    let mut control = before.clone();
    let expected = control.process_block(&signal(1), full).unwrap();
    assert_eq!(path.process_block(&signal(1), full).unwrap(), expected);
    assert_same(&path, &control);

    // Late arithmetic: post-FIR overflow after both neural halves and decimation.
    let mut overflow = record_bytes();
    put(&mut overflow, 0x80c, f32::MAX);
    put(&mut overflow, BETA, 100.0);
    let mut path = self::path(&overflow, "Owned overflow table");
    let before = path.clone();
    assert!(matches!(
        path.process_block(&[1.0; 32], full),
        Err(Error::PostFir(CaptureError::Fir(FirError::Simulation(
            SimulationError::NonFiniteResult { .. }
        ))))
    ));
    assert_same(&path, &before);

    // Strict normal policy: the gain, filters, resampler and alpha admit finite
    // subnormals; the first neural affine refuses one. Parameters and seeds are fine.
    let mut path = self::path(&bytes, "Owned subnormal table");
    path.process_block(&[0.0; 32], full).unwrap();
    let before = path.clone();
    let mut tiny = [0.0; 32];
    tiny[0] = f32::from_bits(0x0070_0000);
    assert!(matches!(
        path.process_block(&tiny, full),
        Err(Error::Neural {
            half: Some(Half::First),
            error: emitted_stack::Error::Leaf {
                leaf: Leaf::FirstAffine0,
                error: NeuralLeafError::Simulation(SimulationError::NonNormalFloat { .. }),
            },
        })
    ));
    assert_same(&path, &before);
    let mut control = before.clone();
    assert_eq!(
        path.process_block(&[0.0; 32], full).unwrap(),
        control.process_block(&[0.0; 32], full).unwrap()
    );
    assert_same(&path, &control);
}

/// A fresh path at the same placements with `path`'s gains and tone parameters.
fn rebuilt(
    record: &RuntimeData<'_>,
    table: CallerOwnedV1ResamplingTable,
    path: &V1EmittedSignalPathReference,
) -> V1EmittedSignalPathReference {
    V1EmittedSignalPathReference::new(
        record,
        table,
        placements(0x8000),
        path.input_gain().clone(),
        path.tones().clone(),
        path.output_gain().clone(),
    )
    .unwrap()
}

#[test]
fn controls_reset_reload_and_instances_keep_owned_bindings() {
    let bytes = record_bytes();
    let record = parse(&bytes);
    let mut path = path(&bytes, "Owned table A");
    let mut other = self::path(&bytes, "Owned table A");
    let fresh = other.clone();
    let full = path.full_budgets();
    for frame in 0..3 {
        path.process_block(&signal(frame), full).unwrap();
    }
    assert_same(&other, &fresh);

    let before = path.clone();
    assert_eq!(
        path.set_input_gain_target(f32::NAN),
        Err(Error::InputGain(CaptureError::NonFinite))
    );
    assert_eq!(
        path.set_output_gain_target(-1.0),
        Err(Error::OutputGain(CaptureError::LinearGainRange))
    );
    assert_eq!(
        path.set_tone_parameters([0.5, 1.5, 0.5]),
        Err(Error::Tones(CaptureError::NormalizedRange))
    );
    assert_same(&path, &before);

    // Reset keeps gain state, tone parameters, table and every image.
    path.set_input_gain_target(0.75).unwrap();
    path.set_tone_parameters([0.3, 0.6, 0.9]).unwrap();
    path.reset();
    assert_same(&path, &rebuilt(&record, table(1.0, "Owned table A"), &path));
    assert_eq!(path.input_gain().target(), 0.75);
    assert_ne!(path.input_gain().current(), 0.125);
    assert_eq!(path.tones().normalized(), [0.3, 0.6, 0.9]);

    // Reload rebinds FIR taps, recompiles changed HP bits, rebuilds alpha, neural
    // and resampling images at the retained placements and ends reset.
    path.process_block(&signal(3), full).unwrap();
    let old = path.clone();
    let mut replacement = bytes.clone();
    for (offset, value) in [
        (ALPHA, 2.0),
        (0x285c, 300.0),
        (0x10, -4.0),
        (0x810, 0.75),
        (0x2828, 0.375),
    ] {
        put(&mut replacement, offset, value);
    }
    let replacement = parse(&replacement);
    path.reload(&replacement, table(0.5, "Owned table B"))
        .unwrap();
    let mut expected = rebuilt(&replacement, table(0.5, "Owned table B"), &path);
    assert_same(&path, &expected);
    assert_eq!(path.table().provenance(), "Owned table B");
    assert_eq!(path.pre_fir().compiled(), old.pre_fir().compiled());
    assert_eq!(path.post_fir().compiled(), old.post_fir().compiled());
    for (new, old) in [
        (path.input_high_pass(), old.input_high_pass()),
        (path.output_high_pass(), old.output_high_pass()),
    ] {
        let (new, old) = (new.compiled(), old.compiled());
        assert_ne!(new.coefficient_bits(), old.coefficient_bits());
        assert_eq!(
            (new.code().origin(), new.layout()),
            (old.code().origin(), old.layout())
        );
    }
    assert_eq!(path.alpha(), 2.0);
    assert_eq!(path.alpha_code().origin(), old.alpha_code().origin());
    assert_ne!(path.alpha_code(), old.alpha_code());
    assert_ne!(
        path.neural().code(Leaf::FirstAffine0),
        old.neural().code(Leaf::FirstAffine0)
    );
    let bound = table(0.5, "Owned table B").coefficients().map(f32::to_bits);
    assert_eq!(path.interpolator().compiled().coefficient_bits(), bound);
    assert_eq!(path.decimator().compiled().coefficient_bits(), bound);
    assert_eq!(
        path.process_block(&signal(4), full).unwrap(),
        expected.process_block(&signal(4), full).unwrap()
    );
    assert_same(&other, &fresh);
    other.process_block(&signal(0), full).unwrap();
}

#[test]
fn construction_and_reload_reject_shape_nonfinite_domain_and_bounds() {
    let bytes = record_bytes();
    let mut path = path(&bytes, "Owned table A");
    path.process_block(&signal(0), path.full_budgets()).unwrap();
    let before = path.clone();
    let build = |record: &RuntimeData<'_>, placements| {
        V1EmittedSignalPathReference::new(
            record,
            table(1.0, "Owned table A"),
            placements,
            gain(0.125, 1.75),
            emitted_tones(TONES),
            gain(2.0, 0.375),
        )
        .err()
    };
    // 1→2→1: same header and tail, seven tensor floats.
    let mut shape = bytes[..0x2848].to_vec();
    for (offset, word) in [(0x2814, 2_u32), (0x2818, 1), (0x281c, 2), (0x2820, 1)] {
        shape[offset..offset + 4].copy_from_slice(&word.to_le_bytes());
    }
    for index in 0..7 {
        put(&mut shape, 0x2824 + 4 * index, 0.25);
    }
    put(&mut shape, 0x2840, 1200.0);
    put(&mut shape, 0x2844, 2.0);
    let edit = |offset, value| {
        let mut bytes = record_bytes();
        put(&mut bytes, offset, value);
        bytes
    };
    let cases = [
        (shape, Error::Record(CaptureError::UnsupportedV1Shape)),
        (edit(0x10, f32::NAN), Error::Record(CaptureError::NonFinite)),
        (edit(0, 44_100.0), Error::Record(CaptureError::SampleRate)),
        (
            edit(ALPHA, 0.5),
            Error::Record(CaptureError::ConstantGainRange),
        ),
        (
            edit(BETA, f32::from_bits(0x3dcc_cccd)),
            Error::Record(CaptureError::ConstantGainRange),
        ),
        (
            edit(0x285c, 24_000.0),
            Error::Record(CaptureError::HighPassDomain),
        ),
        (
            edit(0x2858, f32::from_bits(1)),
            Error::Neural {
                half: None,
                error: emitted_stack::Error::Coefficient(1),
            },
        ),
    ];
    for (bytes, expected) in cases {
        let record = parse(&bytes);
        assert_eq!(build(&record, placements(0x8000)), Some(expected));
        let error = path.reload(&record, table(0.5, "Rejected")).unwrap_err();
        assert_eq!(Some(error), build(&record, placements(0x8000)));
        assert_same(&path, &before);
    }
    let mut bounds = placements(0x8000);
    bounds.alpha.dm_base = DmWordAddress32::new(u32::MAX - 62);
    assert_eq!(
        build(&parse(&bytes), bounds),
        Some(Error::Alpha {
            half: None,
            error: AlphaError::Address(AddressError::DmOverflow),
        })
    );
    assert_eq!(
        CallerOwnedV1ResamplingTable::new([0.5; 16], String::new()).err(),
        Some(CaptureError::ResamplingTableProvenance)
    );
}

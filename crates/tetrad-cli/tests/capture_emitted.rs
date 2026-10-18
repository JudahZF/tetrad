//! `Tetrad capture render-emitted` against the public emitted-path API. Synthetic record,
//! table and audio only; no stock fixtures.

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::Value;
use tetrad_capture::{
    filters::{FirLogicalPlacement, HighPassLogicalPlacement},
    gain::GainAdapter,
    network::emitted_stack::{LeafPlacement, LeafReceipt},
    pipeline::{
        V1StagePlacements,
        emitted::{EmittedV1PathPlacements, V1EmittedSignalPathReference},
    },
    resampling::{CallerOwnedV1ResamplingTable, emitted::ResamplingLogicalPlacement},
    tones::{emitted::ToneLogicalPlacement, emitted_stack::EmittedV1ToneStackReference},
    v1::{Limits, RuntimeData},
};
use tetrad_media::ir::{IrInputFormat, IrLimits, WavPolicy};
use tetrad_sharc::address::{DmWordAddress32, PmWordAddress24};

static NEXT: AtomicU64 = AtomicU64::new(0);

const COEFFICIENTS: [f32; 16] = [
    0.5, 0.25, -0.125, 0.0625, 0.5, 0.25, -0.125, 0.0625, 0.5, 0.25, -0.125, 0.0625, 0.5, 0.25,
    -0.125, 0.0625,
];
const PROVENANCE: &str = "Owned synthetic CLI table; not stock";
const GAINS: [f32; 4] = [0.125, 1.75, 2.0, 0.375];
const TONES: [f32; 3] = [0.9, 0.2, 0.75];
/// Mirrors `MAX_FRAMES` in `src/capture_emitted.rs`.
const MAX_FRAMES: usize = 1500;

fn put(bytes: &mut [u8], offset: usize, value: f32) {
    bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

/// The tetrad-capture emitted-path test record: 1→2→2→1 with an active high-pass.
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
        (0x280c, 1.5),
        (0x2810, 1.75),
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

fn wav_with(format_tag: u16, channels: u16, rate: u32, data: &[u8]) -> Vec<u8> {
    let block = 4 * u32::from(channels);
    let mut bytes = b"RIFF".to_vec();
    bytes.extend((36 + data.len() as u32).to_le_bytes());
    bytes.extend(b"WAVEfmt ");
    bytes.extend(16_u32.to_le_bytes());
    bytes.extend(format_tag.to_le_bytes());
    bytes.extend(channels.to_le_bytes());
    bytes.extend(rate.to_le_bytes());
    bytes.extend((rate * block).to_le_bytes());
    bytes.extend((block as u16).to_le_bytes());
    bytes.extend(32_u16.to_le_bytes());
    bytes.extend(b"data");
    bytes.extend((data.len() as u32).to_le_bytes());
    bytes.extend(data);
    bytes
}

fn wav(samples: &[f32]) -> Vec<u8> {
    let data: Vec<u8> = samples.iter().flat_map(|x| x.to_le_bytes()).collect();
    wav_with(3, 1, 48_000, &data)
}

fn signal(samples: usize) -> Vec<f32> {
    (0..samples)
        .map(|n| ((n * 7) % 23) as f32 / 32.0 - 0.375)
        .collect()
}

fn list(values: &[f32]) -> String {
    values
        .iter()
        .map(f32::to_string)
        .collect::<Vec<_>>()
        .join(",")
}

struct Fixture {
    root: PathBuf,
    record: PathBuf,
    input: PathBuf,
    output: PathBuf,
}

impl Fixture {
    fn new(record: &[u8], input: &[u8]) -> Self {
        let root = std::env::temp_dir().join(format!(
            "tetrad-capture-emitted-cli-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        let fixture = Self {
            record: root.join("record.bin"),
            input: root.join("input.wav"),
            output: root.join("output.wav"),
            root,
        };
        fs::write(&fixture.record, record).unwrap();
        fs::write(&fixture.input, input).unwrap();
        fixture
    }

    /// Every policy flag explicit; `overrides` replaces a flag's value by name.
    fn run(&self, overrides: &[(&str, &str)]) -> Output {
        let [ig_current, ig_target, og_current, og_target] = GAINS.map(|g| g.to_string());
        let mut args = vec![
            ("--resampling-coefficients", list(&COEFFICIENTS)),
            ("--resampling-provenance", PROVENANCE.into()),
            ("--input-gain-current", ig_current),
            ("--input-gain-target", ig_target),
            ("--tones", list(&TONES)),
            ("--output-gain-current", og_current),
            ("--output-gain-target", og_target),
        ];
        for (flag, value) in overrides {
            args.iter_mut().find(|(name, _)| name == flag).unwrap().1 = (*value).into();
        }
        let mut command = Command::new(env!("CARGO_BIN_EXE_tetrad-cli"));
        command
            .args(["capture", "render-emitted", "--record"])
            .arg(&self.record)
            .arg("--input")
            .arg(&self.input)
            .arg("--output")
            .arg(&self.output);
        for (flag, value) in args {
            command.arg(format!("{flag}={value}"));
        }
        command.output().unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.root).unwrap();
    }
}

fn pm(origin: u32) -> PmWordAddress24 {
    PmWordAddress24::new(origin).unwrap()
}

/// Deliberately different from the CLI's private slots: logical placement must not
/// change any output bit.
fn api_path(record: &RuntimeData<'_>) -> V1EmittedSignalPathReference {
    let dm_base = DmWordAddress32::new(0x8000);
    let hp = HighPassLogicalPlacement {
        origin: pm(0x1000),
        dm_base,
    };
    let fir = FirLogicalPlacement {
        origin: pm(0x2000),
        dm_base,
    };
    let resampling = ResamplingLogicalPlacement {
        origin: pm(0x4000),
        dm_base,
    };
    let placements = EmittedV1PathPlacements {
        stages: V1StagePlacements {
            input_hp: hp,
            pre: fir,
            post: fir,
            output_hp: hp,
        },
        interpolate: resampling,
        alpha: LeafPlacement {
            origin: pm(0x5000),
            dm_base,
        },
        neural: std::array::from_fn(|i| LeafPlacement {
            origin: pm(0x6000 + 0x400 * i as u32),
            dm_base,
        }),
        decimate: resampling,
    };
    let gain = |current, target| GainAdapter::new(pm(0x800), dm_base, current, target).unwrap();
    let tone = ToneLogicalPlacement {
        origin: pm(0xc00),
        dm_base,
    };
    V1EmittedSignalPathReference::new(
        record,
        CallerOwnedV1ResamplingTable::new(COEFFICIENTS, PROVENANCE.into()).unwrap(),
        placements,
        gain(GAINS[0], GAINS[1]),
        EmittedV1ToneStackReference::new(TONES, [tone; 3]).unwrap(),
        gain(GAINS[2], GAINS[3]),
    )
    .unwrap()
}

fn refused(result: &Output) -> String {
    assert!(!result.status.success(), "{result:?}");
    String::from_utf8(result.stderr.clone()).unwrap()
}

#[test]
fn render_matches_public_emitted_path_bits_and_receipts() {
    let input = signal(96);
    let record = record_bytes();
    let fixture = Fixture::new(&record, &wav(&input));
    let result = fixture.run(&[]);
    assert!(result.status.success(), "{result:?}");
    let report: Value = serde_json::from_slice(&result.stdout).unwrap();

    let parsed = RuntimeData::parse(&record, Limits::default()).unwrap();
    let mut path = api_path(&parsed);
    let budgets = path.full_budgets();
    let (mut expected, mut steps, mut stores, mut calls) = (Vec::new(), 0, 0, 0);
    for frame in input.chunks_exact(32) {
        let block = path
            .process_block(frame.try_into().unwrap(), budgets)
            .unwrap();
        expected.extend(block.outputs.map(f32::to_bits));
        let r = block.receipts;
        let linear = [r.input_gain, r.input_hp, r.pre, r.interpolate, r.decimate]
            .into_iter()
            .chain(r.alpha)
            .chain([r.post, r.output_hp, r.output_gain])
            .chain(r.tones)
            .map(|o| (o.steps, o.stores));
        let neural = r.neural.into_iter().flatten().map(|leaf| match leaf {
            LeafReceipt::Linear(o) => (o.steps, o.stores),
            LeafReceipt::Adaa(o) => (o.steps, o.stores),
        });
        for (leaf_steps, leaf_stores) in linear.chain(neural) {
            steps += leaf_steps;
            stores += leaf_stores;
            calls += 1;
        }
    }

    // The output is a standard float32 mono 48 kHz WAV holding exactly the API bits.
    let bytes = fs::read(&fixture.output).unwrap();
    let limits = IrLimits {
        max_asset_bytes: bytes.len(),
        max_chunks: 2,
    };
    let decoded =
        tetrad_media::ir::decode(&bytes, IrInputFormat::Wav(WavPolicy::StandardRiff), limits)
            .unwrap();
    assert_eq!(bytes.len(), 44 + 4 * 96);
    assert_eq!(decoded.bits(), expected);
    assert!(expected.iter().any(|&bits| bits != 0));

    assert_eq!(report["input_samples"], 96);
    assert_eq!(report["output_samples"], 96);
    assert_eq!(report["frames"], 3);
    assert_eq!(report["simulation"]["steps"], steps);
    assert_eq!(report["simulation"]["stores"], stores);
    assert_eq!(report["simulation"]["leaf_calls"], calls);
    assert_eq!(calls, 3 * 35);
    assert!(steps <= report["simulation"]["step_budget"].as_u64().unwrap() as usize);
    let gain = &report["policy"]["output_gain"];
    assert_eq!(
        gain["final_current_bits"],
        path.output_gain().current().to_bits()
    );
    assert_eq!(report["policy"]["resampling_provenance"], PROVENANCE);
    for flag in [
        "stock_numeric_parity",
        "device_io",
        "hardware_acceptance",
        "realtime",
    ] {
        assert_eq!(report[flag], false, "{flag}");
    }
}

#[test]
fn refusals_happen_before_output_creation() {
    let refuse = |record: &[u8], input: &[u8], overrides: &[(&str, &str)], message: &str| {
        let fixture = Fixture::new(record, input);
        let stderr = refused(&fixture.run(overrides));
        assert!(stderr.contains(message), "{message}: {stderr}");
        assert!(!fixture.output.exists(), "{message}");
    };
    let samples = signal(64);
    let record = record_bytes();
    let good = wav(&samples);
    let data: Vec<u8> = samples.iter().flat_map(|x| x.to_le_bytes()).collect();
    let mut nan = samples.clone();
    nan[40] = f32::NAN;
    let mut riff_short = good.clone();
    riff_short.truncate(good.len() - 4);
    let inputs = [
        (wav(&samples[..33]), "has 33 samples"),
        (wav(&[]), "has 0 samples"),
        (wav_with(3, 2, 48_000, &data), "channel count"),
        (wav_with(3, 1, 44_100, &data), "sample rate"),
        (wav_with(1, 1, 48_000, &data), "format tag"),
        (wav(&nan), "sample 40 is not finite"),
        (riff_short, "RIFF declares"),
    ];
    for (input, message) in inputs {
        refuse(&record, &input, &[], message);
    }
    let mut trailing = record.clone();
    trailing.push(0);
    refuse(&trailing, &good, &[], "bytes follow");

    let thirteen = list(&COEFFICIENTS[..13]);
    let flags: [(&[(&str, &str)], &str); 5] = [
        (&[("--resampling-coefficients", &thirteen)], "exactly 16"),
        (&[("--resampling-provenance", " ")], "resampling table"),
        (&[("--output-gain-target", "-1")], "output gain: linear"),
        (&[("--tones", "0.5,1.5,0.5")], "tones: normalized"),
        (
            &[
                ("--input-gain-current", "3e38"),
                ("--input-gain-target", "3e38"),
            ],
            "frame 0: neural stack (Some(First)): FirstAdaa0",
        ),
    ];
    for (overrides, message) in flags {
        refuse(&record, &good, overrides, message);
    }

    let fixture = Fixture::new(&record, &good);
    fs::write(&fixture.output, b"keep").unwrap();
    assert!(refused(&fixture.run(&[])).contains("already exists"));
    assert_eq!(fs::read(&fixture.output).unwrap(), b"keep");
}

#[test]
fn workload_cap_refuses_before_simulation() {
    let fixture = Fixture::new(&record_bytes(), &wav(&vec![0.0; 32 * (MAX_FRAMES + 1)]));
    let stderr = refused(&fixture.run(&[]));
    assert!(stderr.contains("1501 frames"), "{stderr}");
    assert!(!fixture.output.exists());
}

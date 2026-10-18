//! Offline render of a mono 48 kHz float32 WAV through the owned emitted V1 signal path
//! (`tetrad_capture::pipeline::emitted`), one 32-sample frame at a time. Every output sample comes
//! from interpreted ISA48 words; the host only stages, checks and encodes. Not stock parity, device
//! I/O or realtime.

use std::path::{Path, PathBuf};

use clap::Args as ClapArgs;
use serde_json::json;
use tetrad_capture::{
    filters::{FirLogicalPlacement, HighPassLogicalPlacement},
    gain::GainAdapter,
    network::emitted_stack::{LeafPlacement, LeafReceipt},
    pipeline::{
        V1StagePlacements,
        emitted::{
            self, EmittedV1PathBudgets, EmittedV1PathPlacements, EmittedV1PathReceipts,
            V1EmittedSignalPathReference,
        },
    },
    resampling::{CallerOwnedV1ResamplingTable, emitted::ResamplingLogicalPlacement},
    tones::{emitted::ToneLogicalPlacement, emitted_stack::EmittedV1ToneStackReference},
    v1::{Limits, RuntimeData},
};
use tetrad_media::ir::{IrInputFormat, IrLimits, WavPolicy};
use tetrad_sharc::address::{DmWordAddress32, PmWordAddress24};

use crate::{Result, io, print_json};

const FRAME: usize = 32;
/// Workload cap: one second at 48 kHz. Release builds run about 2 ms per frame.
const MAX_FRAMES: usize = 1500;
/// Ceiling on `frames * full frame budget` (about 518 600 steps per frame today).
const MAX_STEPS: usize = 800_000_000;
const MAX_SAMPLES: usize = MAX_FRAMES * FRAME;
/// Room for the RIFF header and small unknown chunks such as LIST.
const WAV_LIMITS: IrLimits = IrLimits {
    max_asset_bytes: MAX_SAMPLES * 4 + 64 * 1024,
    max_chunks: 64,
};
const PLACEMENT_POLICY: &str = "cli_owned_logical_v1";
/// One logical PM slot per image; the largest image (interpolator) has 3232 words.
const PM_SLOT_WORDS: u32 = 0x1000;
/// One logical DM base for every leaf: each leaf call stages its own private DM.
const DM_BASE: DmWordAddress32 = DmWordAddress32::new(0);

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{path} already exists; outputs are never replaced")]
    OutputExists { path: PathBuf },
    #[error("input has {samples} samples; need a nonzero multiple of {FRAME} (no padding)")]
    SampleCount { samples: usize },
    #[error("input sample {sample} is not finite")]
    NonFiniteInput { sample: usize },
    #[error(
        "{frames} frames at {frame_budget} steps each exceed the cap of {MAX_FRAMES} frames or {MAX_STEPS} steps"
    )]
    Workload { frames: usize, frame_budget: usize },
    #[error("resampling table: {0}")]
    Table(#[source] tetrad_capture::Error),
    #[error("input gain: {0}")]
    InputGain(#[source] tetrad_capture::Error),
    #[error("tones: {0}")]
    Tones(#[source] tetrad_capture::Error),
    #[error("output gain: {0}")]
    OutputGain(#[source] tetrad_capture::Error),
    #[error("emitted path construction: {}", chain(.0))]
    Path(#[source] emitted::Error),
    #[error("frame {frame}: {}", chain(.source))]
    Frame {
        frame: usize,
        #[source]
        source: emitted::Error,
    },
}

/// The path's stage errors print only the stage and half; the leaf detail is in sources.
fn chain(error: &dyn std::error::Error) -> String {
    let mut text = error.to_string();
    let mut source = error.source();
    while let Some(cause) = source {
        text.push_str(": ");
        text.push_str(&cause.to_string());
        source = cause.source();
    }
    text
}

/// Every numeric policy is explicit; there are no default coefficients, gains or tones.
#[derive(ClapArgs)]
pub struct Args {
    /// Complete decoded V1 runtime-data record (not an encrypted capture).
    #[arg(long)]
    record: PathBuf,
    /// Standard RIFF WAV: IEEE float32, one channel, 48000 Hz, a multiple of 32 samples.
    #[arg(long)]
    input: PathBuf,
    /// Exclusive new float32 WAV; never replaced.
    #[arg(long)]
    output: PathBuf,
    /// Exactly 16 comma-separated caller-owned resampling coefficients.
    #[arg(long, allow_hyphen_values = true, value_parser = parse_list::<16>)]
    resampling_coefficients: [f32; 16],
    /// Nonempty source of the coefficients; reported, never verified.
    #[arg(long)]
    resampling_provenance: String,
    /// Initial linear input gain, finite and nonnegative; no unity default.
    #[arg(long, allow_hyphen_values = true)]
    input_gain_current: f32,
    /// Linear input gain the owned smoother ramps toward.
    #[arg(long, allow_hyphen_values = true)]
    input_gain_target: f32,
    /// Three comma-separated normalized tone parameters, First,Second,Third.
    #[arg(long, allow_hyphen_values = true, value_parser = parse_list::<3>)]
    tones: [f32; 3],
    /// Initial linear output gain, finite and nonnegative; no unity default.
    #[arg(long, allow_hyphen_values = true)]
    output_gain_current: f32,
    /// Linear output gain the owned smoother ramps toward.
    #[arg(long, allow_hyphen_values = true)]
    output_gain_target: f32,
}

fn parse_list<const N: usize>(value: &str) -> std::result::Result<[f32; N], String> {
    let values = value
        .split(',')
        .map(|item| {
            item.trim()
                .parse::<f32>()
                .map_err(|e| format!("{item:?}: {e}"))
        })
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let count = values.len();
    values
        .try_into()
        .map_err(|_| format!("expected exactly {N} comma-separated values, got {count}"))
}

fn slot(index: u32) -> PmWordAddress24 {
    PmWordAddress24::new(index * PM_SLOT_WORDS).expect("23 slots fit PM24")
}

/// Owned logical placements, slots in stage order. Not stock addresses or a memory map.
fn placements() -> EmittedV1PathPlacements {
    let high_pass = |index| HighPassLogicalPlacement {
        origin: slot(index),
        dm_base: DM_BASE,
    };
    let fir = |index| FirLogicalPlacement {
        origin: slot(index),
        dm_base: DM_BASE,
    };
    let resampling = |index| ResamplingLogicalPlacement {
        origin: slot(index),
        dm_base: DM_BASE,
    };
    let leaf = |index| LeafPlacement {
        origin: slot(index),
        dm_base: DM_BASE,
    };
    // Slot 0 is the input gain; 19..=21 the tones and 22 the output gain.
    EmittedV1PathPlacements {
        stages: V1StagePlacements {
            input_hp: high_pass(1),
            pre: fir(2),
            post: fir(17),
            output_hp: high_pass(18),
        },
        interpolate: resampling(3),
        alpha: leaf(4),
        neural: std::array::from_fn(|i| leaf(5 + i as u32)),
        decimate: resampling(16),
    }
}

fn frame_budget(budgets: &EmittedV1PathBudgets) -> usize {
    let stages = budgets.stages;
    let tones = budgets.tones;
    [
        budgets.input_gain,
        stages.input_hp,
        stages.pre,
        budgets.interpolate,
        budgets.decimate,
        stages.post,
        stages.output_hp,
        tones.first,
        tones.second,
        tones.third,
        budgets.output_gain,
    ]
    .into_iter()
    .chain(budgets.alpha)
    .chain(budgets.neural.into_iter().flatten())
    .sum()
}

/// Actual `(steps, stores)` of every leaf call of one frame.
fn outcomes(receipts: &EmittedV1PathReceipts) -> impl Iterator<Item = (usize, usize)> {
    let r = receipts;
    let neural = r.neural.into_iter().flatten().map(|leaf| match leaf {
        LeafReceipt::Linear(outcome) => (outcome.steps, outcome.stores),
        LeafReceipt::Adaa(receipt) => (receipt.steps, receipt.stores),
    });
    [r.input_gain, r.input_hp, r.pre, r.interpolate]
        .into_iter()
        .chain(r.alpha)
        .chain([r.decimate, r.post, r.output_hp])
        .chain(r.tones)
        .chain([r.output_gain])
        .map(|outcome| (outcome.steps, outcome.stores))
        .chain(neural)
}

/// `decode` has already proved one complete, word-aligned `data` chunk at a chunk boundary.
fn data_chunk(bytes: &[u8]) -> &[u8] {
    let mut rest = &bytes[12..];
    loop {
        let size = u32::from_le_bytes(rest[4..8].try_into().expect("4 bytes")) as usize;
        if &rest[..4] == b"data" {
            return &rest[8..8 + size];
        }
        rest = &rest[8 + size + size % 2..];
    }
}

fn read_input(path: &Path) -> Result<(usize, Vec<f32>)> {
    let bytes = io::read_bounded(path, WAV_LIMITS.max_asset_bytes)?;
    let decoded = tetrad_media::ir::decode(
        &bytes,
        IrInputFormat::Wav(WavPolicy::StandardRiff),
        WAV_LIMITS,
    )?;
    let samples = decoded.original_sample_count();
    if samples == 0 || !samples.is_multiple_of(FRAME) {
        return Err(Error::SampleCount { samples }.into());
    }
    let data = data_chunk(&bytes);
    debug_assert_eq!(data.len(), 4 * samples);
    let samples = data
        .chunks_exact(4)
        .map(|word| f32::from_le_bytes(word.try_into().expect("4 bytes")))
        .collect::<Vec<_>>();
    if let Some(sample) = samples.iter().position(|sample| !sample.is_finite()) {
        return Err(Error::NonFiniteInput { sample }.into());
    }
    Ok((bytes.len(), samples))
}

/// Canonical 44-byte header: format 3, mono, 48000 Hz, 32 bits, no extra chunks.
fn encode_wav(samples: &[f32]) -> Vec<u8> {
    let data = u32::try_from(4 * samples.len()).expect("MAX_FRAMES bounds the output");
    let mut bytes = Vec::with_capacity(44 + 4 * samples.len());
    bytes.extend_from_slice(b"RIFF");
    bytes.extend((36 + data).to_le_bytes());
    bytes.extend_from_slice(b"WAVEfmt ");
    bytes.extend(16_u32.to_le_bytes());
    bytes.extend(3_u16.to_le_bytes());
    bytes.extend(1_u16.to_le_bytes());
    bytes.extend(48_000_u32.to_le_bytes());
    bytes.extend(192_000_u32.to_le_bytes());
    bytes.extend(4_u16.to_le_bytes());
    bytes.extend(32_u16.to_le_bytes());
    bytes.extend_from_slice(b"data");
    bytes.extend(data.to_le_bytes());
    bytes.extend(samples.iter().flat_map(|sample| sample.to_le_bytes()));
    bytes
}

fn gain_json(start: &GainAdapter, end: &GainAdapter) -> serde_json::Value {
    json!({
        "current": start.current(), "current_bits": start.current().to_bits(),
        "target": start.target(), "target_bits": start.target().to_bits(),
        "final_current": end.current(), "final_current_bits": end.current().to_bits()
    })
}

/// Validation and simulation refusals precede output creation. Rendering completes
/// in memory first; exclusive write, file sync or JSON errors can occur afterward.
pub fn run(args: Args) -> Result<bool> {
    if args.output.symlink_metadata().is_ok() {
        return Err(Error::OutputExists { path: args.output }.into());
    }
    let record_bytes = io::read_bounded(&args.record, Limits::default().max_input_bytes)?;
    let record = RuntimeData::parse(&record_bytes, Limits::default())?;
    let (input_bytes, input) = read_input(&args.input)?;

    let table =
        CallerOwnedV1ResamplingTable::new(args.resampling_coefficients, args.resampling_provenance)
            .map_err(Error::Table)?;
    let input_gain = GainAdapter::new(
        slot(0),
        DM_BASE,
        args.input_gain_current,
        args.input_gain_target,
    )
    .map_err(Error::InputGain)?;
    let tone_placement = |index| ToneLogicalPlacement {
        origin: slot(index),
        dm_base: DM_BASE,
    };
    let tones = EmittedV1ToneStackReference::new(
        args.tones,
        [tone_placement(19), tone_placement(20), tone_placement(21)],
    )
    .map_err(Error::Tones)?;
    let output_gain = GainAdapter::new(
        slot(22),
        DM_BASE,
        args.output_gain_current,
        args.output_gain_target,
    )
    .map_err(Error::OutputGain)?;
    let initial = V1EmittedSignalPathReference::new(
        &record,
        table,
        placements(),
        input_gain,
        tones,
        output_gain,
    )
    .map_err(Error::Path)?;

    let frames = input.len() / FRAME;
    let budgets = initial.full_budgets();
    let budget = frame_budget(&budgets);
    if frames > MAX_FRAMES || frames.saturating_mul(budget) > MAX_STEPS {
        return Err(Error::Workload {
            frames,
            frame_budget: budget,
        }
        .into());
    }

    let mut path = initial.clone();
    let mut output = Vec::with_capacity(input.len());
    let (mut steps, mut stores, mut calls) = (0, 0, 0);
    for (frame, samples) in input.chunks_exact(FRAME).enumerate() {
        let block = path
            .process_block(samples.try_into().expect("32 samples"), budgets)
            .map_err(|source| Error::Frame { frame, source })?;
        for (leaf_steps, leaf_stores) in outcomes(&block.receipts) {
            steps += leaf_steps;
            stores += leaf_stores;
            calls += 1;
        }
        output.extend(block.outputs);
    }

    let wav = encode_wav(&output);
    io::write_new(&args.output, &wav)?;
    let table = initial.table();
    let [alpha, beta] = record.constant_gains();
    print_json(&json!({
        "record": args.record.to_string_lossy(), "record_bytes": record_bytes.len(),
        "input": args.input.to_string_lossy(), "input_bytes": input_bytes,
        "input_samples": input.len(),
        "output": args.output.to_string_lossy(), "output_bytes": wav.len(),
        "output_samples": output.len(),
        "format": "wav_ieee_float32_mono_48000",
        "frames": frames, "frame_samples": FRAME,
        "simulation": {
            "steps": steps, "stores": stores, "leaf_calls": calls,
            "frame_step_budget": budget, "step_budget": frames * budget,
            "max_frames": MAX_FRAMES, "max_steps": MAX_STEPS,
            "steps_are_cycles": false
        },
        "policy": {
            "resampling_coefficients": table.coefficients(),
            "resampling_coefficient_bits": table.coefficients().map(f32::to_bits),
            "resampling_provenance": table.provenance(),
            "resampling_provenance_verified": false,
            "input_gain": gain_json(initial.input_gain(), path.input_gain()),
            "tones_normalized": initial.tones().normalized(),
            "tones_normalized_bits": initial.tones().normalized().map(f32::to_bits),
            "output_gain": gain_json(initial.output_gain(), path.output_gain()),
            "constant_gains": [alpha, beta],
            "constant_gain_bits": [alpha.to_bits(), beta.to_bits()],
            "constant_gain_use": "serialized_values_direct; stock log/exp reconstruction not modeled",
            "placements": PLACEMENT_POLICY, "pm_slot_words": PM_SLOT_WORDS,
            "dm_base": DM_BASE.get(),
            "initial_state": "zero histories and constructor neural seeds"
        },
        "sample_arithmetic": "emitted_isa48_interpreter",
        "stock_numeric_parity": false, "device_io": false, "hardware_acceptance": false,
        "realtime": false
    }))?;
    Ok(true)
}

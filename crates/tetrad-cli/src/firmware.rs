//! Owned resident firmware: write its LDR image, or boot it in the SHARC
//! simulator. Without `--block`: the gain/mix reference graph from tetrad-runtime's
//! owned firmware test. With `--block`: a path (b) image whose owned blocks run
//! as a mono chain from TDM channel 0 to channel 0 (adapter B).

use std::collections::BTreeMap;
use std::path::PathBuf;

use clap::{Args, Subcommand};
use serde::Deserialize;
use serde_json::json;
use tetrad_blocks::blocks::volume::Volume;
use tetrad_blocks::blocks::{gain::Gain, gate::SimpleGate, geq9::Graphic9, lohicut::LowHighCut};
use tetrad_blocks::verification::standard_frames;
use tetrad_blocks::{BLOCK_SAMPLES, OwnedBlock};
use tetrad_catalog::owned::OwnedModel;
use tetrad_dsp_protocol::packet::{BulkMode, BulkRecord, encode_bulk};
use tetrad_dsp_protocol::{CodecLimits, CoreId, DspCore, MessageType, ModuleSlot};
use tetrad_runtime::firmware::graph::{
    Channel, GraphSpec, KernelKind, NodeSpec, ResidentKernel, SLOT_COUNT, SlotId, Source,
};
use tetrad_runtime::firmware::image::{OwnedBlocks, ResidentConfig, ResidentImage};
use tetrad_runtime::firmware::layout::control;
use tetrad_runtime::firmware::memory::SharcCore;
use tetrad_runtime::firmware::sim::{SimError, SimulatedSoc, TdmBlock};
use tetrad_sharc::machine::{InterruptBit, Machine, ProgramMemory, Stop};

use crate::{Error, Result, io, print_json};

const BUDGET: u64 = 200_000;
const MAX_FRAMES: u32 = 256;
const MAX_SCRIPT_BYTES: usize = 1024 * 1024;
const RING: CoreId = CoreId::Dsp(DspCore::Core0);

#[derive(Subcommand)]
pub enum Command {
    /// Write the resident as a raw ADI LDR file. Not loaded or run.
    Build(Build),
    /// Boot the resident in the SHARC simulator and run TDM frames.
    Simulate(Simulate),
}

#[derive(Args)]
pub struct Graph {
    /// Path (b): an owned block (volume, gain, lohicut, geq9) for the next
    /// module slot (0, 1, ...) of the chain. Repeat for a longer chain.
    /// Without it: the gain/mix reference graph.
    #[arg(long = "block", value_name = "MODEL")]
    blocks: Vec<OwnedModel>,
}

#[derive(Args)]
pub struct Build {
    /// Must not exist.
    #[arg(long)]
    output: PathBuf,
    #[command(flatten)]
    graph: Graph,
}

#[derive(Args)]
pub struct Simulate {
    /// TDM frames to run after boot.
    #[arg(long, default_value = "4", value_parser = clap::value_parser!(u32).range(1..=i64::from(MAX_FRAMES)))]
    frames: u32,
    #[command(flatten)]
    graph: Graph,
    /// Path (b) only: a JSON list of events, for example
    /// `[{"parameter": {"frame": 0, "slot": 0, "index": 0, "value": 0.5}},
    /// {"bypass": {"frame": 8, "slot": 1, "value": 1.0}}]`. Bypass 0.0 is
    /// active, 1.0 bypass, 2.0 bypass with mute. An event is sent on the stock
    /// ring before its frame; the blocks see it in the next frame.
    #[arg(long, requires = "blocks")]
    script: Option<PathBuf>,
}

/// One parameter-script event. Each frame's events go out as one
/// BulkParameterChange and one BulkModelBypass.
#[derive(Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
enum Event {
    Parameter {
        frame: u32,
        slot: u8,
        index: u32,
        value: f32,
    },
    Bypass {
        frame: u32,
        slot: u8,
        value: f32,
    },
}

impl Event {
    fn frame(&self) -> u32 {
        match *self {
            Self::Parameter { frame, .. } | Self::Bypass { frame, .. } => frame,
        }
    }
}

pub fn run(command: Command) -> Result<bool> {
    match command {
        Command::Build(args) => build(args),
        Command::Simulate(args) => simulate(args),
    }
}

fn build(args: Build) -> Result<bool> {
    let image = image(&args.graph.blocks)?;
    let ldr = image.ldr()?;
    io::write_new(&args.output, ldr.bytes())?;
    print_json(&json!({
        "output": args.output.to_string_lossy(),
        "ldr_bytes": ldr.bytes().len(),
        "core": format!("{:?}", image.core()),
        "entry": image.entry().get(),
        "blocks": describe(&args.graph.blocks),
        "code_words": image.code().word_count() + image.block_code().word_count(),
        "initialized_data_words": image.initialized_data().len(),
        "loaded": false,
        "hardware_compatibility": "unverified"
    }))?;
    Ok(true)
}

fn simulate(args: Simulate) -> Result<bool> {
    let blocks = &args.graph.blocks;
    let script: Vec<Event> = match &args.script {
        Some(path) => serde_json::from_slice(&io::read_bounded(path, MAX_SCRIPT_BYTES)?)
            .map_err(Error::Script)?,
        None => Vec::new(),
    };
    let image = image(blocks)?;
    let mut soc = SimulatedSoc::new(image.core());
    let mut program = ProgramMemory::default();
    image.install(&mut program, &mut soc)?;
    let mut machine = Machine::reset();
    machine.set_pc(image.entry().get());
    soc.start();
    run_to_idle(&mut machine, &program, &mut soc)?;

    let signal: Vec<TdmBlock> = if blocks.is_empty() {
        (0..args.frames).map(ramp).collect()
    } else {
        standard_frames()
            .iter()
            .cycle()
            .take(args.frames as usize)
            .map(|frame| tdm_mono(&frame.samples[0]))
            .collect()
    };
    let mut outputs = Vec::new();
    for (index, input) in (0..).zip(&signal) {
        send(
            &mut soc,
            script.iter().filter(|event| event.frame() == index),
        )?;
        let output = soc.transfer_frame(input)?;
        if !soc.interrupt_pending() {
            return Err(Error::NoFrameInterrupt);
        }
        machine.raise(InterruptBit::SECI);
        run_to_idle(&mut machine, &program, &mut soc)?;
        outputs.push(output);
    }

    let execution = "tetrad-runtime SimulatedSoc, not hardware";
    if blocks.is_empty() {
        let frames: Vec<_> = (0..)
            .zip(&outputs)
            .map(|(index, output)| json!({ "index": index, "channel_sums": channel_sums(output) }))
            .collect();
        print_json(&json!({
            "frames_run": args.frames,
            "frames": frames,
            "execution": execution,
            "hardware_compatibility": "unverified"
        }))?;
        return Ok(true);
    }
    let word = |offset| {
        soc.memory()
            .peek(image.layout().control + offset)
            .map_err(SimError::from)
    };
    // Channel 0 of the transmit stream (two frames of latency), as f32 peaks.
    let peaks: Vec<f32> = outputs
        .iter()
        .map(|output| {
            output
                .iter()
                .map(|row| (f64::from(row[0]) / 2_f64.powi(31)).abs() as f32)
                .fold(0.0, f32::max)
        })
        .collect();
    print_json(&json!({
        "frames_run": args.frames,
        "blocks": describe(blocks),
        "events": script.len(),
        "signal": "tetrad-blocks standard frames, channel A, 24-bit TDM on channel 0",
        "output_channel_0_peaks": peaks,
        "cycles_last": word(control::CYCLES_LAST)?,
        "cycles_peak": word(control::CYCLES_PEAK)?,
        "messages": word(control::MESSAGES)?,
        "rejected": word(control::REJECTED)?,
        "unsupported": word(control::UNSUPPORTED)?,
        "fault_vector": word(control::FAULT)?,
        "execution": execution,
        "hardware_compatibility": "unverified"
    }))?;
    Ok(true)
}

/// Publishes one frame's events: a BulkParameterChange (stock encoder) and a
/// BulkModelBypass (`16 + 8n`: records `{slot, f32}`, as the resident reads it).
fn send<'a>(soc: &mut SimulatedSoc, events: impl Iterator<Item = &'a Event>) -> Result<()> {
    let mut parameters = Vec::new();
    let mut bypasses = Vec::new();
    for event in events {
        match *event {
            Event::Parameter {
                slot, index, value, ..
            } => parameters.push(BulkRecord {
                slot: ModuleSlot::try_from(u32::from(slot))?,
                parameter_index: index,
                value_bits: value.to_bits(),
            }),
            Event::Bypass { slot, value, .. } => bypasses.push([u32::from(slot), value.to_bits()]),
        }
    }
    if !parameters.is_empty() {
        let bytes = encode_bulk(
            [0; 16],
            DspCore::Core0,
            BulkMode::Value8,
            &parameters,
            CodecLimits::default(),
        )?;
        soc.publish(RING, &bytes)?;
    }
    if !bypasses.is_empty() {
        let length = 16 + 8 * bypasses.len() as u32;
        let mut bytes = vec![
            MessageType::BulkModelBypass as u8,
            DspCore::Core0 as u8,
            0,
            0,
        ];
        for word in [0, length, bypasses.len() as u32]
            .into_iter()
            .chain(bypasses.into_iter().flatten())
        {
            bytes.extend(word.to_le_bytes());
        }
        soc.publish(RING, &bytes)?;
    }
    Ok(())
}

fn run_to_idle(
    machine: &mut Machine,
    program: &ProgramMemory,
    soc: &mut SimulatedSoc,
) -> Result<()> {
    match machine.run(program, soc, None, BUDGET)? {
        Stop::Idle => Ok(()),
        stop => Err(Error::NotIdle(stop)),
    }
}

/// Channel 0 and channel 1 ramps, scaled by frame index.
fn ramp(frame: u32) -> TdmBlock {
    let scale = 1000 * (frame as i32 + 1);
    let mut block: TdmBlock = [[0; 8]; 32];
    for (sample, row) in block.iter_mut().enumerate() {
        let step = sample as i32 + 1;
        row[0] = scale * step;
        row[1] = scale / 2 * step;
    }
    block
}

/// `samples` as 24-bit words left-justified in TDM channel 0.
fn tdm_mono(samples: &[f32; BLOCK_SAMPLES]) -> TdmBlock {
    let mut block: TdmBlock = [[0; 8]; 32];
    for (row, sample) in block.iter_mut().zip(samples) {
        let q = (sample * 8_388_608.0)
            .round()
            .clamp(-8_388_608.0, 8_388_607.0);
        row[0] = (q as i32) << 8;
    }
    block
}

fn channel_sums(block: &TdmBlock) -> [i64; 8] {
    let mut sums = [0_i64; 8];
    for row in block {
        for (sum, sample) in sums.iter_mut().zip(row) {
            *sum += i64::from(*sample);
        }
    }
    sums
}

fn slot(n: u8) -> SlotId {
    SlotId::new(n).expect("slot is below SLOT_COUNT")
}

fn channel(n: u8) -> Channel {
    Channel::new(n).expect("reference channel is below HARDWARE_CHANNELS")
}

fn body(model: OwnedModel) -> &'static dyn OwnedBlock {
    match model {
        OwnedModel::Volume => &Volume,
        OwnedModel::Gain => &Gain,
        OwnedModel::LowHighCut => &LowHighCut,
        OwnedModel::SimpleGate => &SimpleGate,
        OwnedModel::Graphic9 => &Graphic9,
    }
}

fn describe(blocks: &[OwnedModel]) -> Vec<serde_json::Value> {
    (0..)
        .zip(blocks)
        .map(|(slot, model)| json!({ "slot": slot, "model": model.name(), "module_id": model.stock_id() }))
        .collect()
}

/// The reference graph, or a path (b) chain of `blocks` in slots 0, 1, ...
/// Stereo-state blocks (gate) are refused by the image build.
fn image(blocks: &[OwnedModel]) -> Result<ResidentImage<ResidentKernel>> {
    if blocks.len() > usize::from(SLOT_COUNT) {
        return Err(Error::Usage("a chain holds at most 30 owned blocks"));
    }
    let config = if blocks.is_empty() {
        reference_config()
    } else {
        chain_config(blocks)
    };
    let owned: OwnedBlocks = (0..)
        .zip(blocks)
        .map(|(n, model)| (slot(n), body(*model)))
        .collect();
    Ok(ResidentImage::build_with_blocks(&config, &owned)?)
}

/// in0 → slot 0 → slot 1 → ... → out0, each slot startable by its model id.
fn chain_config(blocks: &[OwnedModel]) -> ResidentConfig<ResidentKernel> {
    let nodes: Vec<_> = (0..)
        .zip(blocks)
        .map(|(n, _)| NodeSpec {
            slot: slot(n),
            kernel: ResidentKernel::Block,
            inputs: vec![match n {
                0 => Source::Input(channel(0)),
                _ => Source::Node(slot(n - 1)),
            }],
        })
        .collect();
    let last = nodes.last().expect("at least one block").slot;
    ResidentConfig {
        core: SharcCore::Core1,
        ring: RING,
        module_ids: (0..)
            .zip(blocks)
            .map(|(n, model)| (slot(n), model.stock_id()))
            .collect(),
        graph: GraphSpec {
            nodes,
            outputs: vec![(channel(0), Source::Node(last))],
        },
    }
}

/// Gain on channel 0 and channel 1, then a mix of both gains into channel 3.
fn reference_config() -> ResidentConfig<ResidentKernel> {
    let node = |n, kernel, inputs| NodeSpec {
        slot: slot(n),
        kernel: ResidentKernel::Builtin(kernel),
        inputs,
    };
    ResidentConfig {
        core: SharcCore::Core1,
        ring: RING,
        graph: GraphSpec {
            nodes: vec![
                node(5, KernelKind::Gain, vec![Source::Input(channel(0))]),
                node(6, KernelKind::Gain, vec![Source::Input(channel(1))]),
                node(
                    7,
                    KernelKind::Mix,
                    vec![Source::Node(slot(5)), Source::Node(slot(6))],
                ),
            ],
            outputs: vec![
                (channel(2), Source::Node(slot(5))),
                (channel(3), Source::Node(slot(7))),
            ],
        },
        module_ids: BTreeMap::from([(slot(5), 22000), (slot(6), 22001)]),
    }
}

//! `Tetrad block`: build an owned block blob to a file, or run it on the modeled SHARC
//! (`tetrad_sharc::machine::Machine`) against its host twin. No device and no audio device.

use std::path::PathBuf;

use clap::Subcommand;
use serde_json::{Value, json};
use tetrad_blocks::verification::{self, VerificationError, VerificationReport};
use tetrad_blocks::{HostReference, ParameterLaw};
use tetrad_catalog::owned::{BlockVisitor, OwnedEntry, OwnedModel};

use crate::{Result, io, print_json};

#[derive(Subcommand)]
pub enum Command {
    /// Write the plaintext stock-ABI blob (`BlobLoad` wire layout) to a new file.
    Build {
        /// volume, gain, lohicut, gate or geq9.
        model: OwnedModel,
        /// Exclusive new file; never replaced.
        #[arg(long)]
        output: PathBuf,
    },
    /// Run the linked blob on the modeled SHARC under the kernel call protocol. The test
    /// signal repeats silence, impulse, full-scale and sine frames; every call must equal the
    /// host twin bit for bit.
    Simulate {
        /// volume, gain, lohicut, gate or geq9.
        model: OwnedModel,
        /// 32-sample stereo frames.
        #[arg(long, default_value_t = 64, value_parser = clap::value_parser!(u32).range(64..=4096))]
        frames: u32,
    },
}

pub fn run(command: Command) -> Result<bool> {
    match command {
        Command::Build { model, output } => {
            let entry = model.build()?;
            let bytes = entry.module.blob.to_bytes()?;
            io::write_new(&output, &bytes)?;
            print_json(&json!({
                "output": output.to_string_lossy(),
                "blob_bytes": bytes.len(),
                "format": "plaintext_module_blob",
                "model": describe(&entry),
                "loaded": false,
                "hardware_compatibility": "unverified",
            }))?;
        }
        Command::Simulate { model, frames } => {
            let entry = model.build()?;
            let report = model.visit(Simulate(frames as usize))?;
            print_json(&json!({
                "model": describe(&entry),
                "frames": report.frames,
                "calls": report.calls,
                "maximum_process_cycles": report.maximum_process_cycles,
                "matches_host_twin": true,
                "executed_on": "modeled SHARC (one step per word; no cache or pipeline)",
                "hardware_compatibility": "unverified",
            }))?;
        }
    }
    Ok(true)
}

struct Simulate(usize);

impl BlockVisitor for Simulate {
    type Output = std::result::Result<VerificationReport, VerificationError>;
    fn visit<B: HostReference>(self, block: &B) -> Self::Output {
        verification::verify_module(block, &verification::scheduled_frames(self.0, &[]))
    }
}

fn describe(entry: &OwnedEntry) -> Value {
    let built = &entry.module.stats;
    let parameters: Vec<_> = entry
        .parameters
        .iter()
        .map(|parameter| {
            let law = match parameter.law {
                ParameterLaw::Range {
                    minimum,
                    maximum,
                    skew,
                } => json!({ "range": { "minimum": minimum, "maximum": maximum, "skew": skew } }),
                ParameterLaw::Switch => json!("switch"),
                ParameterLaw::Choices(names) => json!({ "choices": names }),
                ParameterLaw::Unresolved(reason) => json!({ "unresolved": reason }),
            };
            json!({ "index": parameter.runtime_index, "name": parameter.name, "law": law })
        })
        .collect();
    json!({
        "name": entry.model.name(),
        "stock_id": entry.model.stock_id(),
        "dm_bytes": built.dm_bytes,
        "visa_bytes": built.visa_bytes,
        "isa_bytes": built.isa_bytes,
        "cycles_per_stereo_block": built.cycles_per_stereo_block,
        "reuse_pmco": built.reuse_pmco,
        "fit_cpu_percent": entry.stats.cpu,
        "parameters": parameters,
    })
}

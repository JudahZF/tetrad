use std::path::PathBuf;

use clap::Subcommand;
use serde_json::json;
use tetrad_sharc::{EmitError, address::PmWordAddress24, gain::GAIN_ABI};

use crate::{Result, io, print_json};

#[derive(Subcommand)]
pub enum Command {
    /// Emit the owned nine-word gain leaf. This is not a bootable firmware image.
    Gain {
        /// ISA48 PC word origin, decimal or 0x-prefixed hex; not a physical byte address.
        #[arg(long, default_value = "0", value_parser = parse_origin)]
        origin: u32,
        /// Exclusive new file containing six little-endian bytes per ISA word.
        #[arg(long)]
        output: PathBuf,
    },
}

fn parse_origin(value: &str) -> std::result::Result<u32, String> {
    match value.strip_prefix("0x") {
        Some(hex) => u32::from_str_radix(hex, 16),
        None => value.parse(),
    }
    .map_err(|error| error.to_string())
}

pub fn run(command: Command) -> Result<bool> {
    let Command::Gain { origin, output } = command;
    let origin = PmWordAddress24::new(origin).map_err(EmitError::from)?;
    let code = tetrad_sharc::compile_gain(origin)?;
    let bytes = code.isa48_le_bytes();
    io::write_new(&output, &bytes)?;
    print_json(&json!({
        "output": output.to_string_lossy(), "origin_pc_words": origin.get(),
        "word_count": code.word_count(), "byte_count": bytes.len(),
        "format": "isa48_six_byte_little_endian_section",
        "maximum_samples": GAIN_ABI.maximum_samples,
        "abi": "owned_gain_leaf; I0 input, I1 output, F4 gain, R8 sample count",
        "preconditions": "normal-word DM aliases, scalar RND32, L0=L1=0, valid buffers and owned CALL/RTS convention",
        "stock_runtime_dependency": false, "bootable_image": false,
        "loaded": false, "executed": false, "hardware_compatibility": "unverified"
    }))?;
    Ok(true)
}

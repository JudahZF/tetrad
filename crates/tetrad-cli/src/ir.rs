use std::path::PathBuf;

use clap::{Args, Subcommand, ValueEnum};
use serde_json::json;
use tetrad_media::factory::{DeviceCipher, DeviceSerial, FactoryIrSeed};
use tetrad_media::ir::{IrChannel, IrError, IrInputFormat, IrLimits, IrTransferPlan, WavPolicy};

use crate::{Error, Result, io, print_json};

const LIMITS: IrLimits = IrLimits {
    max_asset_bytes: 16 * 1024 * 1024,
    max_chunks: 4096,
};
const KEY_FILE_MAX_BYTES: usize = 64;

#[derive(Subcommand)]
pub enum Command {
    /// Inspect host IR selection and inert transfer arguments; no sample dump.
    Inspect(Input),
    /// Export selected LE float32 words, not a DSP packet or padded IR buffer.
    ExportPayload {
        #[command(flatten)]
        input: Input,
        /// Exclusive new file, with no header or zero-padding.
        #[arg(long)]
        output: PathBuf,
    },
}

#[derive(Args)]
pub struct Input {
    path: PathBuf,
    /// Explicit classification. Factory files are decrypted and never authenticated.
    #[arg(long, value_enum)]
    format: Format,
    /// Inert transfer destination; does not select a source audio channel.
    #[arg(long, default_value = "0", value_parser = clap::value_parser!(u32).range(0..=1))]
    channel: u32,
    /// Raw 57-byte firmware seed. Required for `factory-bin` only; never printed.
    #[arg(long)]
    seed_file: Option<PathBuf>,
    /// Device serial file (`qc_sn`). Required for `factory-bin` only; never printed.
    #[arg(long)]
    serial_file: Option<PathBuf>,
}

#[derive(Clone, Copy, ValueEnum)]
enum Format {
    RawLeF32,
    WavRecovered38,
    WavStandard,
    FactoryBin,
}

pub fn run(command: Command) -> Result<bool> {
    let (input, output) = match command {
        Command::Inspect(input) => (input, None),
        Command::ExportPayload { input, output } => (input, Some(output)),
    };
    let (format, classification) = match input.format {
        Format::RawLeF32 => (IrInputFormat::RawLeFloat32, "raw_le_f32"),
        Format::WavRecovered38 => (
            IrInputFormat::Wav(WavPolicy::RecoveredHeader38),
            "wav_recovered38",
        ),
        Format::WavStandard => (IrInputFormat::Wav(WavPolicy::StandardRiff), "wav_standard"),
        Format::FactoryBin => (IrInputFormat::FactoryBin, "factory_bin"),
    };
    let cipher = factory_cipher(&input)?;
    let channel = IrChannel::try_from(input.channel)?;
    let bytes = io::read_bounded(&input.path, LIMITS.max_asset_bytes)?;
    let samples = match &cipher {
        Some(cipher) => tetrad_media::ir::decode_with_cipher(&bytes, format, LIMITS, cipher)?,
        None => tetrad_media::ir::decode(&bytes, format, LIMITS)?,
    };
    let transfer = IrTransferPlan::new(&samples, channel);
    if let Some(output) = &output {
        io::write_new(output, transfer.payload())?;
    }
    print_json(&json!({
        "input": input.path.to_string_lossy(),
        "input_bytes": bytes.len(),
        "classification": classification,
        "classification_authenticated": false,
        "original_sample_count": samples.original_sample_count(),
        "selected_sample_count": samples.bits().len(),
        "truncated": samples.is_truncated(),
        "ignored_partial_bytes": samples.ignored_partial_bytes(),
        "sample_rate_hz": samples.source_format().sample_rate_hz(),
        "source_channel_count": samples.source_format().channel_count(),
        "float_bits_preserved": true,
        "max_asset_bytes": LIMITS.max_asset_bytes,
        "max_chunks": LIMITS.max_chunks,
        "transfer_plan": {
            "parameter_id": transfer.parameter_id(),
            "channel": transfer.channel().index(),
            "sample_count": transfer.sample_count(),
            "byte_count": transfer.byte_count(),
            "final_flag": transfer.final_flag(),
            "inert": true,
            "sent": false
        },
        "output": output.as_deref().map(std::path::Path::to_string_lossy),
        "payload_exported": output.is_some(),
        "payload_format": "selected_le_float32_words_no_header_no_padding",
        "audio_validated": false,
        "hardware_compatibility": "unverified"
    }))?;
    Ok(true)
}

/// Builds the factory cipher from key files. Key bytes never reach error text.
fn factory_cipher(input: &Input) -> Result<Option<DeviceCipher>> {
    match (input.format, &input.seed_file, &input.serial_file) {
        (Format::FactoryBin, Some(seed_path), Some(serial_path)) => {
            let seed = io::read_bounded(seed_path, KEY_FILE_MAX_BYTES)?;
            let serial = io::read_bounded(serial_path, KEY_FILE_MAX_BYTES)?;
            let seed = FactoryIrSeed::new(&seed).map_err(IrError::Factory)?;
            let serial = DeviceSerial::parse(&serial).map_err(IrError::Factory)?;
            Ok(Some(DeviceCipher::seed_and_serial(&seed, &serial)))
        }
        (Format::FactoryBin, _, _) => Err(Error::Usage(
            "factory-bin requires --seed-file and --serial-file",
        )),
        (_, None, None) => Ok(None),
        (_, _, _) => Err(Error::Usage(
            "--seed-file and --serial-file apply only to factory-bin",
        )),
    }
}

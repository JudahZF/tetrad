mod block;
mod boot;
mod capture_emitted;
mod engine;
mod firmware;
mod io;
mod ir;
mod library;
mod preset_build;
mod settings;
mod sharc;

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{ArgGroup, Args, Parser, Subcommand};
use qc_format::blob::ModuleBlob;
use serde::Serialize;
use serde_json::json;
use tetrad_catalog::Catalog;
use tetrad_preset::{Limits, ParamValue, PresetDocument};

const MAX_XML_BYTES: usize = 16 * 1024 * 1024;
const MAX_BLOB_BYTES: usize = 32 * 1024 * 1024;

#[derive(Debug, thiserror::Error)]
enum Error {
    #[error(transparent)]
    File(#[from] io::FileError),
    #[error(transparent)]
    Preset(#[from] tetrad_preset::Error),
    #[error(transparent)]
    Catalog(#[from] tetrad_catalog::CatalogError),
    #[error(transparent)]
    Spec(#[from] tetrad_catalog::SpecError),
    #[error(transparent)]
    Blob(#[from] qc_format::FormatError),
    #[error(transparent)]
    Control(#[from] tetrad_control::Error),
    #[error(transparent)]
    Loader(#[from] tetrad_loader::Error),
    #[error(transparent)]
    Dsp(#[from] tetrad_dsp_protocol::Error),
    #[error(transparent)]
    Capture(#[from] tetrad_capture::Error),
    #[error(transparent)]
    CaptureEmitted(#[from] capture_emitted::Error),
    #[error(transparent)]
    Storage(#[from] tetrad_storage::Error),
    #[error(transparent)]
    Sharc(#[from] tetrad_sharc::EmitError),
    #[error(transparent)]
    Ir(#[from] tetrad_media::ir::IrError),
    #[error("source {input} aliases the owned database {database}")]
    DatabaseAlias { database: PathBuf, input: PathBuf },
    #[error("path {path} is reserved for a SQLite sidecar of {database}")]
    DatabaseSidecar { database: PathBuf, path: PathBuf },
    #[error("product {0} has no raw data to export")]
    RawDataAbsent(String),
    #[error("{path}: XML is not UTF-8: {source}")]
    Utf8 {
        path: PathBuf,
        source: std::string::FromUtf8Error,
    },
    #[error("catalog does not contain model ID {0}")]
    Model(u32),
    #[error(transparent)]
    Firmware(#[from] tetrad_runtime::firmware::image::FirmwareError),
    #[error(transparent)]
    Install(#[from] tetrad_runtime::firmware::image::InstallError),
    #[error(transparent)]
    Simulator(#[from] tetrad_runtime::firmware::sim::SimError),
    #[error(transparent)]
    Machine(#[from] tetrad_sharc::machine::MachineError),
    #[error(transparent)]
    OwnedBlock(#[from] tetrad_blocks::BlockError),
    #[error(transparent)]
    OwnedSimulation(#[from] tetrad_blocks::verification::VerificationError),
    #[error(transparent)]
    Import(#[from] tetrad_engine::ImportError),
    #[error(transparent)]
    Compile(#[from] tetrad_engine::CompileError),
    #[error("machine stopped before IDLE: {0:?}")]
    NotIdle(tetrad_sharc::machine::Stop),
    #[error("serial frame did not raise the interrupt the resident waits for")]
    NoFrameInterrupt,
    #[error("{0}")]
    Usage(&'static str),
    #[error("output: {0}")]
    Output(#[from] std::io::Error),
    #[error("JSON output: {0}")]
    Json(#[from] serde_json::Error),
    #[error("event script: {0}")]
    Script(serde_json::Error),
}

type Result<T> = std::result::Result<T, Error>;

#[derive(Parser)]
#[command(
    name = "tetrad-cli",
    version,
    about = "Offline QC data tools. No device access or installation."
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Read or export lossless preset data. Edits are hardware-unverified.
    Preset {
        #[command(subcommand)]
        command: PresetCommand,
    },
    /// Inspect model metadata and independently check DSP blobs.
    Catalog {
        #[command(subcommand)]
        command: CatalogCommand,
    },
    /// Inspect a decrypted DSP blob; no execution or decryption.
    Blob {
        #[command(subcommand)]
        command: BlobCommand,
    },
    /// Inspect saved 128-byte report bodies. No USB/network operations.
    Control {
        #[command(subcommand)]
        command: ControlCommand,
    },
    /// Inspect raw ADI LDR data. Does not execute or load it.
    Ldr {
        #[command(subcommand)]
        command: LdrCommand,
    },
    /// Inspect saved DSP parameter packets. No transmission.
    Dsp {
        #[command(subcommand)]
        command: DspCommand,
    },
    /// Inspect decoded captures or render an explicit owned reference. No device access.
    Capture {
        #[command(subcommand)]
        command: CaptureCommand,
    },
    /// Operate an owned Tetrad library, never a stock CorOS database.
    Library {
        #[command(subcommand)]
        command: library::Command,
    },
    /// Emit owned ISA48 code to a new file. No loading or DSP execution.
    Sharc {
        #[command(subcommand)]
        command: sharc::Command,
    },
    /// Inspect or export explicitly classified host IR samples. No device or DSP access.
    Ir {
        #[command(subcommand)]
        command: ir::Command,
    },
    /// Build or simulate the owned resident firmware. Never loads hardware.
    Firmware {
        #[command(subcommand)]
        command: firmware::Command,
    },
    /// Dump stock settings files from a directory. Reads only, redacts secrets.
    Settings {
        #[command(subcommand)]
        command: settings::Command,
    },
    /// Plan the SOC1 boot from stock images. Prints steps and executes none.
    Boot {
        #[command(subcommand)]
        command: boot::Command,
    },
    /// Import, route, fit and compile a saved preset. Compiles only; nothing is sent.
    Engine {
        #[command(subcommand)]
        command: engine::Command,
    },
    /// Build an owned block blob or run it on the modeled SHARC. Never loads hardware.
    Block {
        #[command(subcommand)]
        command: block::Command,
    },
    /// Print the embedded TOML capability and evidence report.
    Capabilities,
}

#[derive(Subcommand)]
enum PresetCommand {
    Inspect {
        path: PathBuf,
    },
    Validate {
        path: PathBuf,
    },
    /// Prove a byte-identical parse/export, optionally writing a new file.
    Roundtrip {
        path: PathBuf,
        #[arg(long)]
        output: Option<PathBuf>,
    },
    Edit(Edit),
    /// Migrate an old preset; unknown blocks stay in place and are reported.
    Migrate(preset_build::Migrate),
    /// Write a default preset from a decrypted catalog. No DSP compilation.
    New(preset_build::New),
    /// Insert, remove, move or replace a grid block. No DSP compilation.
    EditBlock {
        #[command(subcommand)]
        operation: preset_build::BlockOperation,
    },
}

#[derive(Args)]
#[command(group(ArgGroup::new("operation").required(true).multiple(false).args(["name", "scene_label", "float", "integer", "string"])),
          group(ArgGroup::new("parameter_value").multiple(false).args(["float", "integer", "string"])))]
struct Edit {
    path: PathBuf,
    /// Must not exist. The input is never overwritten.
    #[arg(long)]
    output: PathBuf,
    #[arg(long)]
    name: Option<String>,
    #[arg(long, requires = "scene_label_index")]
    scene_label: Option<String>,
    #[arg(long, requires = "scene_label")]
    scene_label_index: Option<usize>,
    /// Stored list positions, starting at zero, not model or parameter IDs.
    #[arg(long, requires = "parameter_value")]
    chain: Option<usize>,
    #[arg(long, requires = "parameter_value")]
    model: Option<usize>,
    #[arg(long, requires = "parameter_value")]
    parameter: Option<usize>,
    #[arg(long, requires = "parameter_value")]
    scene: Option<usize>,
    #[arg(long, allow_hyphen_values = true, requires_all = ["chain", "model", "parameter", "scene"])]
    float: Option<f32>,
    #[arg(long, allow_hyphen_values = true, requires_all = ["chain", "model", "parameter", "scene"])]
    integer: Option<i32>,
    #[arg(long, requires_all = ["chain", "model", "parameter", "scene"])]
    string: Option<String>,
}

#[derive(Args)]
struct CatalogFiles {
    #[arg(long)]
    model_repo: PathBuf,
    #[arg(long)]
    module_report: PathBuf,
}

#[derive(Subcommand)]
enum CatalogCommand {
    Inspect {
        #[command(flatten)]
        files: CatalogFiles,
        #[arg(long)]
        model: Option<u32>,
    },
    Validate {
        #[command(flatten)]
        files: CatalogFiles,
        /// Explicit directory of decrypted NAME.raw files.
        #[arg(long)]
        blobs: Option<PathBuf>,
    },
}

#[derive(Subcommand)]
enum BlobCommand {
    Inspect { path: PathBuf },
    Validate { path: PathBuf },
}

#[derive(Subcommand)]
enum ControlCommand {
    /// Input is concatenated report bodies, without hidapi report-ID bytes.
    Inspect { path: PathBuf },
}

#[derive(Subcommand)]
enum LdrCommand {
    Inspect { path: PathBuf },
}

#[derive(Subcommand)]
enum DspCommand {
    Parameter { path: PathBuf },
    Bulk { path: PathBuf },
}

#[derive(Subcommand)]
enum CaptureCommand {
    /// Render an explicit owned emitted V1 reference to a new float32 WAV. Not stock parity.
    RenderEmitted(capture_emitted::Args),
    /// Inspects only the leading tag-4 record; reports any following byte count.
    Affine { path: PathBuf },
    /// Inspect a complete decoded V1 runtime-data record; no neural evaluation.
    V1 {
        path: PathBuf,
        /// Fail if runtime values do not pass finite/rate checks. This is not audio parity.
        #[arg(long)]
        validate: bool,
    },
}

fn main() -> ExitCode {
    match run(Cli::parse()) {
        Ok(valid) => {
            if valid {
                ExitCode::SUCCESS
            } else {
                ExitCode::from(2)
            }
        }
        Err(error) => {
            eprintln!("{}", json!({ "error": error.to_string() }));
            ExitCode::FAILURE
        }
    }
}

fn run(cli: Cli) -> Result<bool> {
    match cli.command {
        Command::Capabilities => {
            std::io::stdout()
                .lock()
                .write_all(include_bytes!("../../../docs/capabilities.toml"))?;
        }
        Command::Preset { command } => return preset(command),
        Command::Catalog { command } => return catalog(command),
        Command::Blob { command } => return blob(command),
        Command::Control { command } => return control(command),
        Command::Ldr { command } => return ldr(command),
        Command::Dsp { command } => return dsp(command),
        Command::Capture { command } => return capture(command),
        Command::Library { command } => return library::run(command),
        Command::Sharc { command } => return sharc::run(command),
        Command::Ir { command } => return ir::run(command),
        Command::Firmware { command } => return firmware::run(command),
        Command::Settings { command } => return settings::run(command),
        Command::Boot { command } => return boot::run(command),
        Command::Engine { command } => return engine::run(command),
        Command::Block { command } => return block::run(command),
    }
    Ok(true)
}

fn print_json(value: &impl Serialize) -> Result<()> {
    let mut stdout = std::io::stdout().lock();
    serde_json::to_writer_pretty(&mut stdout, value)?;
    writeln!(stdout)?;
    Ok(())
}

fn preset(command: PresetCommand) -> Result<bool> {
    let validating = matches!(&command, PresetCommand::Validate { .. });
    match command {
        PresetCommand::Inspect { path } | PresetCommand::Validate { path } => {
            let bytes = io::read_bounded(&path, Limits::default().max_bytes)?;
            let document = PresetDocument::parse(&bytes, Limits::default())?;
            let diagnostics = document.validate()?;
            let (summary, summary_error) = match document.summary() {
                Ok(summary) => (Some(summary), None),
                Err(error) => (None, Some(error.to_string())),
            };
            let passed = summary_error.is_none()
                && !diagnostics
                    .iter()
                    .any(|d| d.severity == tetrad_preset::Severity::Error);
            // JSON cannot represent NaN or infinity; the protobuf remains authoritative.
            print_json(&json!({
                "summary": summary, "summary_error": summary_error, "diagnostics": diagnostics,
                "validation_passed": passed, "hardware_compatibility": "unverified",
                "non_finite_json": "non-finite floats appear as null; original protobuf bits are preserved"
            }))?;
            return Ok(!validating || passed);
        }
        PresetCommand::Migrate(args) => return preset_build::migrate(args),
        PresetCommand::New(args) => return preset_build::new(args),
        PresetCommand::EditBlock { operation } => return preset_build::edit_block(operation),
        PresetCommand::Roundtrip { path, output } => {
            let bytes = io::read_bounded(&path, Limits::default().max_bytes)?;
            let document = PresetDocument::parse(&bytes, Limits::default())?;
            let encoded = document.to_bytes();
            let equal = encoded == bytes;
            if equal && let Some(ref output) = output {
                io::write_new(output, &encoded)?;
            }
            print_json(
                &json!({ "input_bytes": bytes.len(), "roundtrip_equal": equal,
                "output": output.as_deref().map(Path::to_string_lossy),
                "hardware_compatibility": "unverified" }),
            )?;
            return Ok(equal);
        }
        PresetCommand::Edit(edit) => {
            let bytes = io::read_bounded(&edit.path, Limits::default().max_bytes)?;
            let mut document = PresetDocument::parse(&bytes, Limits::default())?;
            if let Some(name) = edit.name {
                document.rename(&name)?;
            } else if let (Some(index), Some(label)) = (edit.scene_label_index, edit.scene_label) {
                document.set_scene_label(index, &label)?;
            } else if let (Some(chain), Some(model), Some(parameter), Some(scene)) =
                (edit.chain, edit.model, edit.parameter, edit.scene)
            {
                let value = if let Some(value) = edit.float {
                    ParamValue::Float(value)
                } else if let Some(value) = edit.integer {
                    ParamValue::Int(value)
                } else {
                    ParamValue::String(edit.string.expect("clap requires one parameter value"))
                };
                document.set_parameter(chain, model, parameter, scene, value)?;
            } else {
                unreachable!("clap requires exactly one complete edit operation");
            }
            let encoded = document.to_bytes();
            io::write_new(&edit.output, &encoded)?;
            print_json(
                &json!({ "output": edit.output.to_string_lossy(), "input_bytes": bytes.len(), "output_bytes": encoded.len(),
                "changed": encoded != bytes, "hardware_compatibility": "unverified",
                "note": "Identity and migration flags are preserved; no DSP compilation or hardware import was performed" }),
            )?;
        }
    }
    Ok(true)
}

fn read_xml(path: &Path) -> Result<String> {
    String::from_utf8(io::read_bounded(path, MAX_XML_BYTES)?).map_err(|source| Error::Utf8 {
        path: path.to_owned(),
        source,
    })
}

fn catalog(command: CatalogCommand) -> Result<bool> {
    let files = match &command {
        CatalogCommand::Inspect { files, .. } | CatalogCommand::Validate { files, .. } => files,
    };
    let catalog = Catalog::parse(
        &read_xml(&files.model_repo)?,
        &read_xml(&files.module_report)?,
    )?;
    match command {
        CatalogCommand::Inspect {
            model: Some(id), ..
        } => {
            let model = catalog.model(id).ok_or(Error::Model(id))?;
            print_json(
                &json!({ "model": model, "module": catalog.modules.get(&id), "hardware_compatibility": "unverified" }),
            )?;
        }
        CatalogCommand::Inspect { model: None, .. } => {
            print_json(
                &json!({ "summary": catalog.summary(), "diagnostics": catalog.diagnostics() }),
            )?;
        }
        CatalogCommand::Validate { blobs, .. } => {
            let diagnostics = catalog.diagnostics();
            let report = blobs
                .as_deref()
                .map(|path| catalog.inspect_blobs(path))
                .transpose()?;
            let passed = !diagnostics
                .iter()
                .any(|d| d.severity == tetrad_catalog::Severity::Error)
                && !report
                    .as_ref()
                    .is_some_and(tetrad_catalog::BlobReport::has_errors);
            print_json(
                &json!({ "summary": catalog.summary(), "diagnostics": diagnostics, "blobs": report,
                "blob_checks_requested": blobs.is_some(), "validation_passed": passed,
                "hardware_compatibility": "unverified" }),
            )?;
            return Ok(passed);
        }
    }
    Ok(true)
}

fn capture(command: CaptureCommand) -> Result<bool> {
    match command {
        CaptureCommand::RenderEmitted(args) => return capture_emitted::run(args),
        CaptureCommand::Affine { path } => {
            let limits = tetrad_capture::affine::Limits::default();
            let bytes = io::read_bounded(&path, limits.max_record_bytes)?;
            let (record, consumed) = tetrad_capture::affine::AffineRecord::parse(&bytes, limits)?;
            let (inputs, outputs) = record.dimensions();
            print_json(
                &json!({ "inputs": inputs, "outputs": outputs, "record_bytes": consumed,
                "following_bytes": bytes.len() - consumed,
                "reference_shape_supported": outputs == 1 && inputs >= 4 && inputs.is_multiple_of(4),
                "tensor_bits_preserved": true, "inference_performed": false,
                "scope": "decoded_tag4_record_prefix_only" }),
            )?;
        }
        CaptureCommand::V1 { path, validate } => {
            let limits = tetrad_capture::v1::Limits::default();
            let bytes = io::read_bounded(&path, limits.max_input_bytes)?;
            let record = tetrad_capture::v1::RuntimeData::parse(&bytes, limits)?;
            let validation_error = record
                .validate_runtime_values()
                .err()
                .map(|e| e.to_string());
            let passed = validation_error.is_none();
            let sizes: Vec<_> = record.layer_sizes().collect();
            let layers: Vec<_> = record
                .layers()
                .map(|layer| {
                    json!({
                        "inputs": layer.inputs, "outputs": layer.outputs,
                        "weight_count": layer.weights.len(), "bias_count": layer.biases.len()
                    })
                })
                .collect();
            print_json(&json!({
                "record_bytes": record.as_bytes().len(),
                "sample_rate": record.sample_rate(), "sample_rate_bits": record.sample_rate().to_bits(),
                "pre_filter_length": record.pre_coefficients().len(),
                "post_filter_length": record.post_coefficients().len(),
                "constant_gains": record.constant_gains(),
                "constant_gain_bits": record.constant_gains().map(f32::to_bits),
                "high_pass": record.high_pass(), "high_pass_bits": record.high_pass().map(f32::to_bits),
                "layer_sizes": sizes, "layers": layers,
                "fixed_neural_shape_supported": sizes == [1, 2, 2, 1],
                "validation_passed": passed, "validation_error": validation_error,
                "tensor_bits_preserved": true, "inference_performed": false,
                "output_coefficient": "beta comes from post-gain; direct use is an opt-in mathematical reference, not stock log/exp equivalence",
                "scope": "decoded_v1_runtime_data_only", "hardware_compatibility": "unverified"
            }))?;
            return Ok(!validate || passed);
        }
    }
    Ok(true)
}

fn dsp(command: DspCommand) -> Result<bool> {
    use tetrad_dsp_protocol::packet::{BulkParameterChange, ParameterChange};
    match command {
        DspCommand::Parameter { path } => {
            let bytes = io::read_bounded(&path, 20)?;
            let packet = ParameterChange::decode(&bytes)?;
            let target = packet.target();
            print_json(
                &json!({ "type": 2, "core": target.core as u8, "slot": target.slot.index(),
                "parameter_index": packet.parameter_index(), "value_bits": packet.value().to_bits(),
                "value": packet.value(), "transmitted": false }),
            )?;
        }
        DspCommand::Bulk { path } => {
            let limits = tetrad_dsp_protocol::CodecLimits::default();
            let bytes = io::read_bounded(&path, limits.max_message_bytes)?;
            let packet = BulkParameterChange::decode(&bytes, limits)?;
            let records: Vec<_> = packet
                .records()
                .map(|record| {
                    json!({
                        "slot": record.slot.index(), "parameter_index": record.parameter_index,
                        "value_bits": record.value_bits, "value": f32::from_bits(record.value_bits)
                    })
                })
                .collect();
            print_json(&json!({ "type": 29, "core": packet.core() as u8,
                "mode_word": packet.mode_word(), "records": records, "transmitted": false }))?;
        }
    }
    Ok(true)
}

fn ldr(command: LdrCommand) -> Result<bool> {
    let LdrCommand::Inspect { path } = command;
    let limits = tetrad_loader::Limits::default();
    let bytes = io::read_bounded(&path, limits.max_stream_bytes)?;
    let stream = tetrad_loader::RawLdr::parse(&bytes, limits)?;
    let blocks: Vec<_> = stream
        .blocks()
        .iter()
        .map(|block| {
            json!({
                "offset": block.offset, "signature": block.header.signature() as u8,
                "flags": block.header.flags(), "target_address": block.header.target_address(),
                "byte_count": block.header.byte_count(), "argument": block.header.argument(),
                "fill": block.header.is_fill()
            })
        })
        .collect();
    print_json(
        &json!({ "byte_len": bytes.len(), "block_count": blocks.len(), "blocks": blocks,
        "total_declared_bytes": stream.total_declared_bytes(), "loaded": false,
        "entry_derivation": "not_implemented", "wrapper_support": "raw_ldr_only" }),
    )?;
    Ok(true)
}

fn control(command: ControlCommand) -> Result<bool> {
    let ControlCommand::Inspect { path } = command;
    let bytes = io::read_bounded(&path, MAX_XML_BYTES)?;
    let mut decoder = tetrad_control::Reassembler::new(tetrad_control::Limits::default());
    let mut messages = Vec::new();
    for report in bytes.chunks(tetrad_control::REPORT_BYTES) {
        if let Some(message) = decoder.push(report)? {
            messages.push(json!({
                "message_type": message.message_type,
                "transform": message.transform,
                "decompress": message.decompress,
                "opaque_trailer": message.opaque,
                "payload_bytes": message.payload.len(),
                "payload_processing_error": message.plain_payload().err().map(|error| error.to_string())
            }));
        }
    }
    decoder.finish()?;
    print_json(
        &json!({ "reports": bytes.len() / tetrad_control::REPORT_BYTES, "messages": messages,
        "transport": "saved_report_bodies_only", "actions_performed": false }),
    )?;
    Ok(true)
}

fn blob(command: BlobCommand) -> Result<bool> {
    let (path, validate) = match command {
        BlobCommand::Inspect { path } => (path, false),
        BlobCommand::Validate { path } => (path, true),
    };
    let bytes = io::read_bounded(&path, MAX_BLOB_BYTES)?;
    let blob = ModuleBlob::parse(&bytes)?;
    let semantic_error = tetrad_catalog::validate_blob(&blob)
        .err()
        .map(|error| error.to_string());
    let roundtrip_equal = blob.to_bytes()? == bytes;
    let sections: Vec<_> = blob
        .contents
        .iter()
        .map(|section| {
            json!({
                "section": format!("{:?}", section.section), "byte_len": section.bytes.len(),
                "bytes_per_address_unit": section.section.unit_bytes()
            })
        })
        .collect();
    let relocations: Vec<_> = blob
        .relocations
        .iter()
        .map(|section| {
            json!({
                "section": format!("{:?}", section.section), "word_count": section.words.len()
            })
        })
        .collect();
    let ranges: Vec<_> = blob.ranges.iter().map(|range| json!({
        "section": format!("{:?}", range.section), "start": range.start, "end_inclusive": range.end
    })).collect();
    print_json(
        &json!({ "byte_len": bytes.len(), "entry": blob.entry, "sections": sections,
        "relocations": relocations, "ranges": ranges, "roundtrip_equal": roundtrip_equal,
        "semantic_valid": semantic_error.is_none(), "semantic_error": semantic_error,
        "hardware_execution": "not_performed" }),
    )?;
    Ok(!validate || (roundtrip_equal && semantic_error.is_none()))
}
